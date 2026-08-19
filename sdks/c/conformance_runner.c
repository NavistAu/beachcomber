/*
 * conformance_runner.c — Protocol conformance runner for the beachcomber C
 * binding.
 *
 * This binding links directly against libbeachcomber's C ABI (bc_* in
 * beachcomber.h) — there is no hand-written socket/protocol layer in this
 * directory to drive instead (see Phase 5 of
 * docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md). Every
 * bc_* call returns a caller-owned JSON envelope string
 * (`{"ok":true,"data":...}` / `{"ok":false,"error":{"kind":...,"message":...}}`);
 * this runner parses those envelopes with runner_json.[ch], a parser scoped
 * to this file alone and not shipped as part of the SDK (see
 * runner_json.h for why).
 *
 * Loads fixture JSON files from tests/conformance (relative path resolved
 * from CONFORMANCE_DIR env var or the default relative location), spawns
 * the daemon binary (COMB_BIN env var or argv[1]), drives ops through the
 * ABI, and validates expect blocks directly against the returned envelope.
 *
 * Usage:
 *   COMB_BIN=/path/to/comb ./conformance_runner [conformance_dir]
 *
 * Exit code: 0 = all pass, non-zero = failures.
 *
 * Fixture format (see tests/conformance/README.md):
 *   {
 *     "name": "...",
 *     "description": "...",
 *     "setup": [ { "op": "put", "args": {...} } ],
 *     "test":  { "op": "get",  "args": {...} },
 *     "virtual": { "provider.field": "expr" },   // resolve fixtures only
 *     "env": { "VAR": "value" },                 // resolve fixtures only
 *     "cwd": "/some/path",                       // resolve fixtures only
 *     "expect": {
 *       "status": "hit"|"miss"|"ok"|"error",
 *       "data_type": "string"|"number"|"bool"|"object"|"array"|"null",
 *       "data_equals": <json>,
 *       "data_as_text": "...",
 *       "data_contains_field": "...",
 *       "data_field_equals": { "field": "...", "value": <json> },
 *       "age_ms_present": true|false,
 *       "stale": true|false,
 *       "error_contains": "..."
 *     }
 *   }
 */

#define _POSIX_C_SOURCE 200809L

#include "beachcomber.h"
#include "runner_json.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#include <stdint.h>
#include <dirent.h>
#include <unistd.h>
#include <errno.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <signal.h>

/* -------------------------------------------------------------------------
 * Limits and constants
 * ---------------------------------------------------------------------- */

#define MAX_FIXTURES     256
#define MAX_FIXTURE_SZ   (128 * 1024)
#define SOCK_PATH_MAX    256
#define STARTUP_RETRIES  30
#define STARTUP_SLEEP_US 100000  /* 100 ms */
#define CLIENT_TIMEOUT_MS 5000   /* socket read/write timeout per op */
#define WATCH_TIMEOUT_MS  3000   /* bc_watch_next budget for the initial event */

/* -------------------------------------------------------------------------
 * Minimal test framework
 * ---------------------------------------------------------------------- */

static int g_pass = 0;
static int g_fail = 0;
static int g_skip = 0;

static void report_pass(const char *name) {
    printf("  PASS  %s\n", name);
    g_pass++;
}

static void report_fail(const char *name, const char *reason, ...) {
    va_list ap;
    va_start(ap, reason);
    fprintf(stderr, "  FAIL  %s: ", name);
    vfprintf(stderr, reason, ap);
    fprintf(stderr, "\n");
    va_end(ap);
    g_fail++;
}

static void report_skip(const char *name, const char *op) {
    printf("  SKIP  %s: unsupported op %s\n", name, op);
    g_skip++;
}

/* Every op the fixture format defines is implemented directly over the ABI
 * (bc_resolve gives us "resolve" — the one op earlier phases of this
 * refactor had no binding for). Kept as an explicit allow-list, rather than
 * assuming, so a future op the ABI doesn't yet cover skips loudly instead
 * of crashing into a NULL. */
static int op_supported(const char *op) {
    static const char *supported[] = {
        "hello", "get", "refresh", "put", "status", "context", "watch",
        "introspect", "resolve", NULL
    };
    for (int i = 0; supported[i]; i++) {
        if (strcmp(op, supported[i]) == 0) return 1;
    }
    return 0;
}

/* Return the first unsupported op name in the fixture (setup or test), or
 * NULL if every op is supported. */
static const char *unsupported_op(const json_node_t *root) {
    json_node_t *setup_arr = json_get(root, "setup");
    if (setup_arr && setup_arr->type == JSON_ARRAY) {
        for (json_node_t *item = setup_arr->children; item; item = item->next) {
            const char *op = json_as_str(json_get(item, "op"));
            if (op && !op_supported(op)) return op;
        }
    }
    json_node_t *test_node = json_get(root, "test");
    const char *test_op = test_node ? json_as_str(json_get(test_node, "op")) : NULL;
    if (test_op && !op_supported(test_op)) return test_op;
    return NULL;
}

/* -------------------------------------------------------------------------
 * File helpers
 * ---------------------------------------------------------------------- */

/* Read an entire file into a malloc'd buffer. Returns NULL on error. */
static char *read_file(const char *path) {
    FILE *f = fopen(path, "r");
    if (!f) return NULL;

    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);

    if (sz < 0 || sz > MAX_FIXTURE_SZ) {
        fclose(f);
        return NULL;
    }

    char *buf = (char *)malloc((size_t)sz + 1);
    if (!buf) { fclose(f); return NULL; }

    size_t rd = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    buf[rd] = '\0';
    return buf;
}

/* Check if path is a regular file with a .json extension. */
static int is_json_file(const char *name) {
    size_t len = strlen(name);
    return len > 5 && strcmp(name + len - 5, ".json") == 0;
}

/* -------------------------------------------------------------------------
 * Fixture discovery
 * ---------------------------------------------------------------------- */

typedef struct {
    char path[1024];
} fixture_path_t;

static int collect_fixtures(const char *dir,
                             fixture_path_t *out, int cap) {
    int count = 0;
    DIR *d = opendir(dir);
    if (!d) return 0;

    struct dirent *entry;
    while ((entry = readdir(d)) != NULL && count < cap) {
        if (entry->d_name[0] == '.') continue;

        char subpath[1024];
        snprintf(subpath, sizeof(subpath), "%s/%s", dir, entry->d_name);

        struct stat st;
        if (stat(subpath, &st) != 0) continue;

        if (S_ISDIR(st.st_mode)) {
            /* Recurse one level */
            DIR *sd = opendir(subpath);
            if (!sd) continue;
            struct dirent *se;
            while ((se = readdir(sd)) != NULL && count < cap) {
                if (se->d_name[0] == '.') continue;
                if (!is_json_file(se->d_name)) continue;
                snprintf(out[count].path, sizeof(out[count].path),
                         "%s/%s", subpath, se->d_name);
                count++;
            }
            closedir(sd);
        } else if (S_ISREG(st.st_mode) && is_json_file(entry->d_name)) {
            snprintf(out[count].path, sizeof(out[count].path),
                     "%s", subpath);
            count++;
        }
    }
    closedir(d);
    return count;
}

/* -------------------------------------------------------------------------
 * JSON re-serialisation (fixture args -> bc_* JSON string arguments)
 * ---------------------------------------------------------------------- */

/*
 * serialise_node: serialise a parsed json_node_t back into a malloc'd JSON
 * string. Used to turn fixture "data" (put), "env" and "virtual" (resolve)
 * blocks into the JSON text bc_put / bc_resolve expect.
 */
static char *serialise_node(const json_node_t *node) {
    if (!node) return NULL;

    switch (node->type) {
        case JSON_NULL:
            return strdup("null");
        case JSON_BOOL:
            return strdup(node->val.boolean ? "true" : "false");
        case JSON_INT: {
            char buf[32];
            snprintf(buf, sizeof(buf), "%lld", (long long)node->val.integer);
            return strdup(buf);
        }
        case JSON_FLOAT: {
            /* %g alone drops the fractional part for a whole-number float
             * (42.0 -> "42"), which would re-enter the daemon as an integer
             * literal and silently change its JSON_INT/JSON_FLOAT type (see
             * src/provider/mod.rs Value::from_json_at, which keys off
             * exactly that textual distinction). Force a decimal point back
             * in when %g produced neither one nor an exponent, so a
             * round-tripped whole-number float stays a float on the wire. */
            char buf[64];
            snprintf(buf, sizeof(buf), "%g", node->val.floating);
            if (!strpbrk(buf, ".eEnN")) { /* no '.', exponent, inf, or nan */
                strncat(buf, ".0", sizeof(buf) - strlen(buf) - 1);
            }
            return strdup(buf);
        }
        case JSON_STRING: {
            /* Re-escape for JSON, byte-for-byte over str_len rather than
             * NUL-terminated iteration: a JSON \u0000 escape decodes to a
             * literal NUL byte mid-string (see json_node_t.str_len), and a
             * strlen()/`for (;*p;)` walk would silently truncate the value
             * there before it ever reaches bc_put. JSON also requires every
             * control character (including NUL) to be escaped, so emit
             * \u00XX for anything below 0x20 that doesn't have a named
             * escape. */
            size_t len = node->str_len;
            char *buf = (char *)malloc(len * 6 + 3);
            if (!buf) return NULL;
            size_t i = 0;
            buf[i++] = '"';
            for (size_t j = 0; j < len; j++) {
                unsigned char c = (unsigned char)node->val.string[j];
                if (c == '"')       { buf[i++] = '\\'; buf[i++] = '"'; }
                else if (c == '\\') { buf[i++] = '\\'; buf[i++] = '\\'; }
                else if (c == '\n') { buf[i++] = '\\'; buf[i++] = 'n'; }
                else if (c == '\r') { buf[i++] = '\\'; buf[i++] = 'r'; }
                else if (c == '\t') { buf[i++] = '\\'; buf[i++] = 't'; }
                else if (c < 0x20)  { i += (size_t)sprintf(buf + i, "\\u%04x", c); }
                else                { buf[i++] = (char)c; }
            }
            buf[i++] = '"';
            buf[i]   = '\0';
            return buf;
        }
        case JSON_OBJECT: {
            /* Build {key:val,...} */
            size_t cap = 256;
            char *buf = (char *)malloc(cap);
            if (!buf) return NULL;
            size_t pos = 0;
            buf[pos++] = '{';
            json_node_t *child = node->children;
            int first = 1;
            while (child) {
                char *cv = serialise_node(child);
                if (!cv) { free(buf); return NULL; }
                size_t klen = child->key ? strlen(child->key) : 0;
                size_t vlen = strlen(cv);
                size_t need = klen + vlen + 8;
                if (pos + need >= cap) {
                    cap = (cap + need) * 2;
                    char *nb = (char *)realloc(buf, cap);
                    if (!nb) { free(cv); free(buf); return NULL; }
                    buf = nb;
                }
                if (!first) buf[pos++] = ',';
                first = 0;
                buf[pos++] = '"';
                if (child->key) {
                    memcpy(buf + pos, child->key, klen);
                    pos += klen;
                }
                buf[pos++] = '"';
                buf[pos++] = ':';
                memcpy(buf + pos, cv, vlen);
                pos += vlen;
                free(cv);
                child = child->next;
            }
            buf[pos++] = '}';
            buf[pos]   = '\0';
            return buf;
        }
        case JSON_ARRAY: {
            size_t cap = 256;
            char *buf = (char *)malloc(cap);
            if (!buf) return NULL;
            size_t pos = 0;
            buf[pos++] = '[';
            json_node_t *child = node->children;
            int first = 1;
            while (child) {
                char *cv = serialise_node(child);
                if (!cv) { free(buf); return NULL; }
                size_t vlen = strlen(cv);
                if (pos + vlen + 4 >= cap) {
                    cap = (cap + vlen) * 2;
                    char *nb = (char *)realloc(buf, cap);
                    if (!nb) { free(cv); free(buf); return NULL; }
                    buf = nb;
                }
                if (!first) buf[pos++] = ',';
                first = 0;
                memcpy(buf + pos, cv, vlen);
                pos += vlen;
                free(cv);
                child = child->next;
            }
            buf[pos++] = ']';
            buf[pos]   = '\0';
            return buf;
        }
    }
    return NULL;
}

/* Deep equality check on two json_node_t trees (used for data_equals /
 * data_field_equals). */
static int nodes_equal(const json_node_t *a, const json_node_t *b) {
    if (!a || !b) return a == b;
    if (a->type != b->type) return 0;
    switch (a->type) {
        case JSON_NULL:   return 1;
        case JSON_BOOL:   return a->val.boolean == b->val.boolean;
        case JSON_INT:    return a->val.integer == b->val.integer;
        case JSON_FLOAT:  return a->val.floating == b->val.floating;
        case JSON_STRING:
            /* memcmp, not strcmp: a unicode escape for codepoint U+0000
             * decodes to a literal NUL byte mid-buffer, which strcmp would
             * stop comparing at -- two strings differing only after that
             * byte would wrongly compare equal. str_len is the true
             * decoded length (see json_node_t.str_len). */
            return a->str_len == b->str_len &&
                   memcmp(a->val.string, b->val.string, a->str_len) == 0;
        case JSON_ARRAY:
        case JSON_OBJECT: {
            if (a->n_children != b->n_children) return 0;
            json_node_t *ac = a->children;
            json_node_t *bc = b->children;
            while (ac && bc) {
                if (a->type == JSON_OBJECT) {
                    if (!ac->key || !bc->key) return 0;
                    if (strcmp(ac->key, bc->key) != 0) return 0;
                }
                if (!nodes_equal(ac, bc)) return 0;
                ac = ac->next;
                bc = bc->next;
            }
            return ac == NULL && bc == NULL;
        }
    }
    return 0;
}

/* -------------------------------------------------------------------------
 * Envelope helpers
 * ---------------------------------------------------------------------- */

/* Returns 1 if a bc_* envelope string's top-level "ok" is true, 0 otherwise
 * (including on a NULL or unparseable envelope). */
static int envelope_ok(const char *json) {
    if (!json) return 0;
    json_node_t *root = json_parse(json);
    if (!root) return 0;
    json_node_t *ok = json_get(root, "ok");
    int result = ok && ok->type == JSON_BOOL && ok->val.boolean;
    json_free(root);
    return result;
}

/* -------------------------------------------------------------------------
 * Daemon lifecycle
 * ---------------------------------------------------------------------- */

typedef struct {
    pid_t pid;
    char  sock_path[SOCK_PATH_MAX];
} daemon_proc_t;

/* Builds a bc_client_new() options_json for `sock_path`. autostart is always
 * false: this runner owns daemon lifecycle explicitly (see spawn_daemon /
 * stop_daemon) and must not race the library's own auto-spawn. */
static void build_options_json(char *buf, size_t buf_len,
                                const char *sock_path, uint64_t timeout_ms) {
    snprintf(buf, buf_len,
             "{\"socket_path\":\"%s\",\"autostart\":false,\"timeout_ms\":%llu}",
             sock_path, (unsigned long long)timeout_ms);
}

static daemon_proc_t spawn_daemon(const char *bin, const char *sock_path) {
    daemon_proc_t dp;
    dp.pid = -1;
    dp.sock_path[0] = '\0';

    /* Remove any stale socket */
    unlink(sock_path);

    /* Flush before fork(): stdio buffers unflushed output copied into the
     * child are re-flushed when it later calls freopen() on stdout/stderr,
     * duplicating every banner line printed so far into the log once per
     * fixture. */
    fflush(NULL);

    pid_t pid = fork();
    if (pid < 0) return dp;

    if (pid == 0) {
        /* Child: exec the daemon with the temp socket path */
        /* Suppress daemon output */
        freopen("/dev/null", "w", stdout);
        freopen("/dev/null", "w", stderr);

        char sock_arg[SOCK_PATH_MAX + 16];
        snprintf(sock_arg, sizeof(sock_arg), "--socket=%s", sock_path);
        execl(bin, bin, "daemon", sock_arg, NULL);
        /* exec failed */
        _exit(127);
    }

    /* Parent: wait for the socket to appear and answer a hello. */
    dp.pid = pid;
    snprintf(dp.sock_path, sizeof(dp.sock_path), "%s", sock_path);

    for (int i = 0; i < STARTUP_RETRIES; i++) {
        usleep(STARTUP_SLEEP_US);

        char opts[SOCK_PATH_MAX + 64];
        build_options_json(opts, sizeof(opts), sock_path, 1000);
        BcClient *probe = bc_client_new(opts);
        char *resp = bc_hello(probe);
        int ready = envelope_ok(resp);
        bc_string_free(resp);
        bc_client_free(probe);
        if (ready) return dp;

        /* Check if child died already */
        int wstatus = 0;
        pid_t wp = waitpid(pid, &wstatus, WNOHANG);
        if (wp == pid) {
            dp.pid = -1;
            return dp;
        }
    }

    /* Timed out */
    kill(pid, SIGTERM);
    waitpid(pid, NULL, 0);
    dp.pid = -1;
    return dp;
}

static void stop_daemon(daemon_proc_t *dp) {
    if (dp->pid <= 0) return;
    kill(dp->pid, SIGTERM);
    waitpid(dp->pid, NULL, 0);
    unlink(dp->sock_path);
    dp->pid = -1;
}

/* -------------------------------------------------------------------------
 * Op driver: run a single op descriptor over the ABI, return the raw
 * envelope string (caller frees with bc_string_free). Every bc_ and
 * bc_session_ call already returns exactly this shape, so this function is
 * a dispatch table, not a translation layer.
 * ---------------------------------------------------------------------- */

/*
 * default_cwd: the directory `resolve` fixtures get when they don't declare
 * their own "cwd" — this run's shared per-run temp directory (real,
 * existing, but otherwise inert; path-expression fixtures that care about a
 * specific cwd always declare one explicitly).
 */
static char *run_op(BcClient *client, BcSession *session,
                     const json_node_t *op_node, const json_node_t *fixture_root,
                     const char *default_cwd) {
    if (!op_node) return NULL;

    const char *op_str = json_as_str(json_get(op_node, "op"));
    if (!op_str) return NULL;

    json_node_t *args = json_get(op_node, "args");

    const char *key  = args ? json_as_str(json_get(args, "key"))  : NULL;
    const char *path = args ? json_as_str(json_get(args, "path")) : NULL;

    if (strcmp(op_str, "hello") == 0) {
        return bc_hello(client);
    }

    if (strcmp(op_str, "get") == 0) {
        uint32_t flags = 0;
        json_node_t *force = args ? json_get(args, "force") : NULL;
        json_node_t *wait  = args ? json_get(args, "wait")  : NULL;
        if (force && force->type == JSON_BOOL && force->val.boolean) flags |= BC_GET_FORCE;
        if (wait  && wait->type  == JSON_BOOL && wait->val.boolean)  flags |= BC_GET_WAIT;
        return bc_session_get(session, key ? key : "", path, flags);
    }

    if (strcmp(op_str, "refresh") == 0) {
        return bc_refresh(client, key ? key : "", path);
    }

    if (strcmp(op_str, "context") == 0) {
        return bc_session_set_context(session, path ? path : "");
    }

    if (strcmp(op_str, "put") == 0) {
        json_node_t *data_node = args ? json_get(args, "data") : NULL;
        char *data_json = data_node ? serialise_node(data_node) : strdup("null");
        char *result = bc_session_put(session, key ? key : "", data_json, NULL, path);
        free(data_json);
        return result;
    }

    if (strcmp(op_str, "status") == 0) {
        return bc_status(client);
    }

    if (strcmp(op_str, "introspect") == 0) {
        const char *subject = args ? json_as_str(json_get(args, "subject")) : NULL;
        char opts[64];
        const char *opts_ptr = NULL;
        if (args) {
            json_node_t *d = json_get(args, "duration_secs");
            int ok2 = 0;
            int64_t v = d ? json_as_int(d, &ok2) : 0;
            if (ok2 && v > 0) {
                snprintf(opts, sizeof(opts), "{\"duration_secs\":%lld}", (long long)v);
                opts_ptr = opts;
            }
        }
        return bc_introspect(client, subject ? subject : "daemon", opts_ptr);
    }

    if (strcmp(op_str, "watch") == 0) {
        BcWatch *w = bc_watch_open(client, key ? key : "", path);
        char *result = bc_watch_next(w, WATCH_TIMEOUT_MS);
        bc_watch_free(w);
        return result;
    }

    if (strcmp(op_str, "resolve") == 0) {
        /* cwd / env / virtual are fixture-level fields, not op args — see
         * tests/conformance/README.md's "resolve" section. */
        const char *cwd = json_as_str(json_get(fixture_root, "cwd"));
        if (!cwd) cwd = default_cwd;

        json_node_t *env_node = json_get(fixture_root, "env");
        char *env_json = (env_node && env_node->type == JSON_OBJECT)
                              ? serialise_node(env_node)
                              : NULL;

        json_node_t *virtual_node = json_get(fixture_root, "virtual");
        char *overrides_json = (virtual_node && virtual_node->type == JSON_OBJECT)
                                    ? serialise_node(virtual_node)
                                    : NULL;

        char *result = bc_resolve(client, key ? key : "", cwd, env_json, overrides_json);
        free(env_json);
        free(overrides_json);
        return result;
    }

    return NULL;
}

/* -------------------------------------------------------------------------
 * Expectation checker — reads the bc_* envelope string directly.
 * ---------------------------------------------------------------------- */

static const char *stringify_scalar(const json_node_t *n, char *buf, size_t buf_len) {
    if (!n) return NULL;
    switch (n->type) {
        case JSON_STRING: return n->val.string;
        case JSON_INT:    snprintf(buf, buf_len, "%lld", (long long)n->val.integer); return buf;
        case JSON_FLOAT:  snprintf(buf, buf_len, "%g", n->val.floating); return buf;
        case JSON_BOOL:   return n->val.boolean ? "true" : "false";
        default:          return NULL;
    }
}

/* Every expectation kind documented in tests/conformance/README.md. A
 * fixture using a key outside this set fails loudly rather than being
 * silently ignored — the whole point of this runner is to catch a fixture
 * asserting something the harness doesn't actually check. */
static int is_known_expect_key(const char *key) {
    static const char *known[] = {
        "status", "data_type", "data_equals", "data_as_text",
        "data_contains_field", "data_field_equals", "age_ms_present",
        "stale", "error_contains",
    };
    for (size_t i = 0; i < sizeof(known) / sizeof(known[0]); i++) {
        if (strcmp(key, known[i]) == 0) return 1;
    }
    return 0;
}

static int check_expect(const char *name, const json_node_t *expect,
                        const char *op_str, char *envelope_json) {
    if (!expect) {
        report_pass(name);
        return 1;
    }

    if (expect->type == JSON_OBJECT) {
        for (json_node_t *c = expect->children; c; c = c->next) {
            if (c->key && !is_known_expect_key(c->key)) {
                report_fail(name, "fixture uses unknown expectation key \"%s\" — the runner has no check for it", c->key);
                return 0;
            }
        }
    }

    if (!envelope_json) {
        report_fail(name, "op produced no response");
        return 0;
    }

    json_node_t *root = json_parse(envelope_json);
    if (!root) {
        report_fail(name, "unparseable envelope: %s", envelope_json);
        return 0;
    }

    json_node_t *ok_node = json_get(root, "ok");
    int ok = ok_node && ok_node->type == JSON_BOOL && ok_node->val.boolean;

    const char *status = json_as_str(json_get(expect, "status"));

    if (status && strcmp(status, "error") == 0) {
        if (ok) {
            report_fail(name, "expected status=error but ok=true");
            json_free(root);
            return 0;
        }
        const char *ec = json_as_str(json_get(expect, "error_contains"));
        if (ec) {
            json_node_t *err = json_get(root, "error");
            const char *msg = err ? json_as_str(json_get(err, "message")) : NULL;
            if (!msg || strstr(msg, ec) == NULL) {
                report_fail(name, "expected error containing \"%s\", got: \"%s\"",
                            ec, msg ? msg : "(null)");
                json_free(root);
                return 0;
            }
        }
        report_pass(name);
        json_free(root);
        return 1;
    }

    if (!ok) {
        json_node_t *err = json_get(root, "error");
        const char *msg = err ? json_as_str(json_get(err, "message")) : NULL;
        report_fail(name, "expected ok=true but ok=false (error: %s)",
                    msg ? msg : "(none)");
        json_free(root);
        return 0;
    }

    /* Only "get" and "watch" (event outcome) wrap their payload as
     * {"data":{"data":<value>,"age_ms":...,"stale":...}}; every other op's
     * envelope "data" field IS the value. */
    json_node_t *data = json_get(root, "data");
    json_node_t *value = data;
    json_node_t *age_node = NULL;
    json_node_t *stale_node = NULL;
    int is_nested = strcmp(op_str, "get") == 0 || strcmp(op_str, "watch") == 0;
    if (is_nested && data && data->type == JSON_OBJECT) {
        value = json_get(data, "data");
        age_node = json_get(data, "age_ms");
        stale_node = json_get(data, "stale");
    }

    int is_null = !value || value->type == JSON_NULL;

    if (status) {
        if (strcmp(status, "hit") == 0 && is_null) {
            report_fail(name, "expected status=hit but data is null/absent");
            json_free(root);
            return 0;
        }
        if (strcmp(status, "miss") == 0 && !is_null) {
            report_fail(name, "expected status=miss but data is present");
            json_free(root);
            return 0;
        }
        /* "ok" asserts only ok=true, already checked above. */
    }

    /* data_type */
    const char *data_type = json_as_str(json_get(expect, "data_type"));
    if (data_type) {
        int type_ok = 0;
        if (!is_null) {
            if      (strcmp(data_type, "string") == 0) type_ok = value->type == JSON_STRING;
            else if (strcmp(data_type, "number") == 0) type_ok = value->type == JSON_INT ||
                                                                   value->type == JSON_FLOAT;
            else if (strcmp(data_type, "bool")   == 0) type_ok = value->type == JSON_BOOL;
            else if (strcmp(data_type, "object") == 0) type_ok = value->type == JSON_OBJECT;
            else if (strcmp(data_type, "array")  == 0) type_ok = value->type == JSON_ARRAY;
            else if (strcmp(data_type, "null")   == 0) type_ok = value->type == JSON_NULL;
        } else if (strcmp(data_type, "null") == 0) {
            type_ok = 1;
        }
        if (!type_ok) {
            report_fail(name, "data_type mismatch: expected %s", data_type);
            json_free(root);
            return 0;
        }
    }

    /* data_contains_field */
    const char *dcf = json_as_str(json_get(expect, "data_contains_field"));
    if (dcf) {
        int has = value && value->type == JSON_OBJECT && json_get(value, dcf) != NULL;
        if (!has) {
            report_fail(name, "data_contains_field: field \"%s\" absent", dcf);
            json_free(root);
            return 0;
        }
    }

    /* data_as_text */
    const char *dat = json_as_str(json_get(expect, "data_as_text"));
    if (dat) {
        char buf[64];
        const char *got = stringify_scalar(value, buf, sizeof(buf));
        if (!got || strcmp(got, dat) != 0) {
            report_fail(name, "data_as_text: expected \"%s\", got \"%s\"",
                        dat, got ? got : "(null)");
            json_free(root);
            return 0;
        }
    }

    /* data_equals */
    json_node_t *de = json_get(expect, "data_equals");
    if (de) {
        if (!nodes_equal(value, de)) {
            report_fail(name, "data_equals mismatch");
            json_free(root);
            return 0;
        }
    }

    /* data_field_equals: { "field": "...", "value": <json> } */
    json_node_t *dfe = json_get(expect, "data_field_equals");
    if (dfe) {
        const char *fname = json_as_str(json_get(dfe, "field"));
        json_node_t *fval = json_get(dfe, "value");
        if (fname && fval) {
            json_node_t *field = value ? json_get(value, fname) : NULL;
            if (!field || !nodes_equal(field, fval)) {
                report_fail(name, "data_field_equals: field \"%s\" mismatch", fname);
                json_free(root);
                return 0;
            }
        }
    }

    /* age_ms_present. A fresh hit legitimately has age_ms=0, so this checks
     * key presence (age_node != NULL) rather than a truthy/nonzero value —
     * combresult_to_json emits "age_ms" on both Hit and Miss. */
    json_node_t *amp = json_get(expect, "age_ms_present");
    if (amp && amp->type == JSON_BOOL) {
        int want = amp->val.boolean;
        int got = age_node != NULL;
        if (want && !got) {
            report_fail(name, "age_ms_present: expected age_ms to be present");
            json_free(root);
            return 0;
        }
        if (!want && got) {
            report_fail(name, "age_ms_present: expected age_ms to be absent");
            json_free(root);
            return 0;
        }
    }

    /* stale */
    json_node_t *stale_expect = json_get(expect, "stale");
    if (stale_expect && stale_expect->type == JSON_BOOL) {
        int want = stale_expect->val.boolean;
        int got = stale_node && stale_node->type == JSON_BOOL && stale_node->val.boolean;
        if (want != got) {
            report_fail(name, "stale: expected %d, got %d", want, got);
            json_free(root);
            return 0;
        }
    }

    report_pass(name);
    json_free(root);
    return 1;
}

/* -------------------------------------------------------------------------
 * Fixture runner
 * ---------------------------------------------------------------------- */

static int run_fixture(const char *fixture_path,
                       const char *comb_bin,
                       int fixture_index,
                       const char *sock_dir) {
    char *src = read_file(fixture_path);
    if (!src) {
        fprintf(stderr, "  SKIP  %s: cannot read file\n", fixture_path);
        return 1;
    }

    json_node_t *root = json_parse(src);
    free(src);
    if (!root) {
        fprintf(stderr, "  SKIP  %s: invalid JSON\n", fixture_path);
        return 1;
    }

    const char *name = json_as_str(json_get(root, "name"));
    if (!name) name = fixture_path;

    const char *skip_op = unsupported_op(root);
    if (skip_op) {
        report_skip(name, skip_op);
        json_free(root);
        return 1;
    }

    /* Spawn a fresh daemon for this fixture. The socket lives under a
     * private per-run directory (sock_dir), not bare /tmp: the daemon's
     * singleton lock hardens the pid file's parent directory to mode 0700
     * on every start (src/singleton/mod.rs), and /tmp itself is root-owned,
     * so that chmod is rejected with EPERM before the daemon ever binds. */
    char sock_path[SOCK_PATH_MAX];
    snprintf(sock_path, sizeof(sock_path),
             "%s/comb_conform_%d.sock", sock_dir, fixture_index);

    daemon_proc_t dp = spawn_daemon(comb_bin, sock_path);
    if (dp.pid < 0) {
        fprintf(stderr, "  SKIP  %s: daemon failed to start (%s)\n",
                name, comb_bin);
        json_free(root);
        return 1;
    }

    char opts[SOCK_PATH_MAX + 64];
    build_options_json(opts, sizeof(opts), sock_path, CLIENT_TIMEOUT_MS);
    BcClient *client = bc_client_new(opts);
    BcSession *session = bc_session_open(client);

    /* Setup ops run on the same session as the test op, matching the
     * fixture format's "setup ops happen on the same connection as the
     * test op" contract (needed for "context" to be observable). */
    json_node_t *setup_arr = json_get(root, "setup");
    if (setup_arr && setup_arr->type == JSON_ARRAY) {
        for (json_node_t *item = setup_arr->children; item; item = item->next) {
            char *r = run_op(client, session, item, root, sock_dir);
            bc_string_free(r);
        }
    }

    json_node_t *test_node = json_get(root, "test");
    const char *op_str = test_node ? json_as_str(json_get(test_node, "op")) : "";
    char *result = run_op(client, session, test_node, root, sock_dir);

    json_node_t *expect = json_get(root, "expect");
    int passed = check_expect(name, expect, op_str, result);

    bc_string_free(result);
    bc_session_close(session);
    bc_client_free(client);
    stop_daemon(&dp);
    json_free(root);

    return passed;
}

/* -------------------------------------------------------------------------
 * Main
 * ---------------------------------------------------------------------- */

int main(int argc, char *argv[]) {
    /* Locate conformance fixtures directory */
    char conf_dir_buf[SOCK_PATH_MAX];
    const char *conf_dir = NULL;
    if (argc > 1) {
        conf_dir = argv[1];
    } else {
        conf_dir = getenv("CONFORMANCE_DIR");
        if (!conf_dir) {
            /* Default: relative to the *binary's* location (sdks/c/ -> repo
             * root -> tests/conformance/), not the caller's cwd. The two
             * differ whenever this is invoked from outside sdks/c/ — e.g.
             * `sdks/c/conformance_runner` from the repo root, exactly how
             * this runner is normally invoked — and in a git worktree,
             * cwd-relative "../../" silently lands in the wrong checkout's
             * tests/conformance/ instead of failing loudly. */
            char argv0_copy[SOCK_PATH_MAX];
            snprintf(argv0_copy, sizeof(argv0_copy), "%s", argv[0]);
            char *slash = strrchr(argv0_copy, '/');
            if (slash) {
                *slash = '\0';
                snprintf(conf_dir_buf, sizeof(conf_dir_buf),
                         "%s/../../tests/conformance", argv0_copy);
            } else {
                snprintf(conf_dir_buf, sizeof(conf_dir_buf),
                         "../../tests/conformance");
            }
            conf_dir = conf_dir_buf;
        }
    }

    /* Locate daemon binary */
    const char *comb_bin = getenv("COMB_BIN");
    if (!comb_bin) {
        fprintf(stderr,
                "COMB_BIN not set. Set it to the path of the comb binary.\n"
                "Example: COMB_BIN=/path/to/comb %s\n", argv[0]);
        return 2;
    }

    printf("Conformance runner\n");
    printf("  Fixtures: %s\n", conf_dir);
    printf("  Daemon:   %s\n", comb_bin);
    printf("  Library:  %s\n\n", bc_version());

    /* Collect fixture paths */
    fixture_path_t fixtures[MAX_FIXTURES];
    int n = collect_fixtures(conf_dir, fixtures, MAX_FIXTURES);
    if (n == 0) {
        fprintf(stderr, "No fixtures found in %s\n", conf_dir);
        return 2;
    }

    printf("Found %d fixtures\n\n", n);

    const char *base_tmpdir = getenv("TMPDIR");
    if (!base_tmpdir || base_tmpdir[0] == '\0') base_tmpdir = "/tmp";
    char sock_dir[SOCK_PATH_MAX];
    snprintf(sock_dir, sizeof(sock_dir), "%s/beachcomber-conformance-%d",
             base_tmpdir, (int)getpid());
    if (mkdir(sock_dir, 0700) != 0) {
        fprintf(stderr, "mkdir %s failed: %s\n", sock_dir, strerror(errno));
        return 2;
    }

    for (int i = 0; i < n; i++) {
        run_fixture(fixtures[i].path, comb_bin, i, sock_dir);
    }

    char rm_cmd[SOCK_PATH_MAX + 8];
    snprintf(rm_cmd, sizeof(rm_cmd), "rm -rf %s", sock_dir);
    system(rm_cmd);

    printf("\n=== Results: %d passed, %d failed, %d skipped ===\n", g_pass, g_fail, g_skip);
    return g_fail == 0 ? 0 : 1;
}
