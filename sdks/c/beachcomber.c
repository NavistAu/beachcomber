/*
 * beachcomber.c — C client library for the beachcomber daemon
 */

#include "beachcomber.h"
#include "json.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/un.h>

/* -------------------------------------------------------------------------
 * Internal types
 * ---------------------------------------------------------------------- */

struct comb_client {
    int fd;               /* Connected Unix-domain socket fd, or -1 */
    char *socket_path;    /* Resolved socket path (owned) */
};

struct comb_result {
    int      ok;          /* 1 = server said ok:true */
    int      is_hit;      /* 1 = data was present */
    char    *error;       /* Error string (owned, or NULL) */
    char    *raw_json;    /* Full response line (owned) */
    uint64_t age_ms;
    int      stale;
    json_node_t *parsed;  /* Parsed response root (owned, or NULL) */
    json_node_t *data;    /* Pointer into parsed tree for the "data" node */
};

/* -------------------------------------------------------------------------
 * Internal helpers
 * ---------------------------------------------------------------------- */

/* Write a NUL-terminated string to a char buffer of given capacity.
 * Returns 1 on success, 0 if too small. */
static int safe_strcpy(char *dst, size_t cap, const char *src) {
    if (!dst || cap == 0) return 0;
    size_t n = strlen(src);
    if (n >= cap) return 0;
    memcpy(dst, src, n + 1);
    return 1;
}

/* Allocate and return a copy of src. Aborts on OOM. */
static char *xstrdup(const char *src) {
    if (!src) return NULL;
    size_t n = strlen(src);
    char *out = (char *)malloc(n + 1);
    if (!out) abort();
    memcpy(out, src, n + 1);
    return out;
}

/* Allocate a comb_result_t. Aborts on OOM. */
static comb_result_t *result_alloc(void) {
    comb_result_t *r = (comb_result_t *)calloc(1, sizeof(comb_result_t));
    if (!r) abort();
    return r;
}

/* Build an error result (network / parse error). */
static comb_result_t *result_error(const char *msg) {
    comb_result_t *r = result_alloc();
    r->ok       = 0;
    r->error    = xstrdup(msg);
    r->raw_json = xstrdup("");
    return r;
}

/* -------------------------------------------------------------------------
 * Socket path discovery
 * ---------------------------------------------------------------------- */

char *comb_socket_path(char *dst, size_t dst_len) {
    if (!dst || dst_len == 0) return NULL;

    /* 1. $XDG_RUNTIME_DIR/beachcomber/sock */
    const char *xdg = getenv("XDG_RUNTIME_DIR");
    if (xdg && *xdg) {
        char candidate[4096];
        snprintf(candidate, sizeof(candidate), "%s/beachcomber/sock", xdg);
        /* Check existence with a non-blocking connect attempt */
        int fd = socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd >= 0) {
            struct sockaddr_un addr;
            memset(&addr, 0, sizeof(addr));
            addr.sun_family = AF_UNIX;
            strncpy(addr.sun_path, candidate, sizeof(addr.sun_path) - 1);
            if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0) {
                close(fd);
                if (!safe_strcpy(dst, dst_len, candidate)) return NULL;
                return dst;
            }
            close(fd);
        }
    }

    /* 2. $TMPDIR/beachcomber-<uid>/sock */
    uid_t uid = getuid();
    const char *tmpdir = getenv("TMPDIR");
    if (!tmpdir || !*tmpdir) tmpdir = "/tmp";

    char candidate[4096];
    snprintf(candidate, sizeof(candidate),
             "%s/beachcomber-%u/sock", tmpdir, (unsigned)uid);
    if (!safe_strcpy(dst, dst_len, candidate)) return NULL;
    return dst;
}

/* -------------------------------------------------------------------------
 * Connection
 * ---------------------------------------------------------------------- */

comb_client_t *comb_connect_path(const char *socket_path) {
    if (!socket_path || *socket_path == '\0') return NULL;

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return NULL;

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    if (strlen(socket_path) >= sizeof(addr.sun_path)) {
        close(fd);
        return NULL;
    }
    strncpy(addr.sun_path, socket_path, sizeof(addr.sun_path) - 1);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(fd);
        return NULL;
    }

    comb_client_t *c = (comb_client_t *)malloc(sizeof(comb_client_t));
    if (!c) { close(fd); return NULL; }
    c->fd          = fd;
    c->socket_path = xstrdup(socket_path);
    return c;
}

comb_client_t *comb_connect(void) {
    char path[4096];
    if (!comb_socket_path(path, sizeof(path))) return NULL;
    return comb_connect_path(path);
}

void comb_disconnect(comb_client_t *client) {
    if (!client) return;
    if (client->fd >= 0) close(client->fd);
    free(client->socket_path);
    free(client);
}

/* -------------------------------------------------------------------------
 * Low-level send / receive
 * ---------------------------------------------------------------------- */

/*
 * Write a complete JSON line (with trailing '\n') to the socket.
 * Returns 0 on success, -1 on error.
 */
static int send_line(int fd, const char *line) {
    size_t len = strlen(line);
    /* We need room for the trailing newline */
    char *buf = (char *)malloc(len + 2);
    if (!buf) return -1;
    memcpy(buf, line, len);
    buf[len]     = '\n';
    buf[len + 1] = '\0';

    size_t sent = 0;
    while (sent < len + 1) {
        ssize_t n = write(fd, buf + sent, len + 1 - sent);
        if (n < 0) {
            if (errno == EINTR) continue;
            free(buf);
            return -1;
        }
        sent += (size_t)n;
    }
    free(buf);
    return 0;
}

/*
 * Read a newline-terminated response from the socket.
 * Returns a malloc'd NUL-terminated string (without the newline), or NULL.
 */
static char *recv_line(int fd) {
    size_t cap = 4096;
    size_t len = 0;
    char *buf  = (char *)malloc(cap);
    if (!buf) return NULL;

    for (;;) {
        char ch;
        ssize_t n = read(fd, &ch, 1);
        if (n < 0) {
            if (errno == EINTR) continue;
            free(buf);
            return NULL;
        }
        if (n == 0) {
            /* EOF */
            if (len == 0) { free(buf); return NULL; }
            break;
        }
        if (ch == '\n') break;

        if (len + 1 >= cap) {
            cap *= 2;
            char *nb = (char *)realloc(buf, cap);
            if (!nb) { free(buf); return NULL; }
            buf = nb;
        }
        buf[len++] = ch;
    }

    buf[len] = '\0';
    return buf;
}

/* -------------------------------------------------------------------------
 * Response parsing
 * ---------------------------------------------------------------------- */

static comb_result_t *parse_response(char *raw_json) {
    comb_result_t *r = result_alloc();
    r->raw_json = raw_json; /* takes ownership */

    json_node_t *root = json_parse(raw_json);
    if (!root) {
        r->ok    = 0;
        r->error = xstrdup("failed to parse server response");
        return r;
    }
    r->parsed = root;

    /* "ok" field */
    json_node_t *ok_node = json_get(root, "ok");
    r->ok = 0;
    if (ok_node) {
        r->ok = json_as_bool(ok_node, NULL);
    }

    if (!r->ok) {
        /* "error" field */
        json_node_t *err_node = json_get(root, "error");
        const char *err_str   = json_as_str(err_node);
        r->error = xstrdup(err_str ? err_str : "unknown error");
        return r;
    }

    /* "data" field */
    json_node_t *data_node = json_get(root, "data");
    if (data_node && data_node->type != JSON_NULL) {
        r->is_hit = 1;
        r->data   = data_node;
    }

    /* "age_ms" */
    json_node_t *age_node = json_get(root, "age_ms");
    if (age_node) {
        int ok2 = 0;
        int64_t v = json_as_int(age_node, &ok2);
        if (ok2 && v >= 0) r->age_ms = (uint64_t)v;
    }

    /* "stale" */
    json_node_t *stale_node = json_get(root, "stale");
    if (stale_node) {
        int ok2 = 0;
        r->stale = json_as_bool(stale_node, &ok2);
    }

    return r;
}

/* Send request JSON, read response, return parsed result. */
static comb_result_t *do_request(comb_client_t *client,
                                  const char *request_json) {
    if (!client || client->fd < 0)
        return result_error("not connected");

    if (send_line(client->fd, request_json) != 0)
        return result_error("failed to send request");

    char *raw = recv_line(client->fd);
    if (!raw)
        return result_error("failed to read response");

    return parse_response(raw);
}

/* -------------------------------------------------------------------------
 * JSON building helpers (no external deps — hand-rolled)
 * ---------------------------------------------------------------------- */

/*
 * Escape a string for embedding in JSON.
 * Returns a malloc'd string (caller frees), or NULL on OOM.
 */
static char *json_escape(const char *s) {
    if (!s) return NULL;
    size_t cap = strlen(s) * 2 + 3;
    char *out = (char *)malloc(cap);
    if (!out) return NULL;
    size_t i = 0;
    out[i++] = '"';
    for (const char *p = s; *p; p++) {
        unsigned char c = (unsigned char)*p;
        /* Grow if needed — worst case 6 bytes per char (\u00XX) */
        if (i + 8 >= cap) {
            cap = cap * 2 + 8;
            char *nb = (char *)realloc(out, cap);
            if (!nb) { free(out); return NULL; }
            out = nb;
        }
        switch (c) {
            case '"':  out[i++] = '\\'; out[i++] = '"';  break;
            case '\\': out[i++] = '\\'; out[i++] = '\\'; break;
            case '\n': out[i++] = '\\'; out[i++] = 'n';  break;
            case '\r': out[i++] = '\\'; out[i++] = 'r';  break;
            case '\t': out[i++] = '\\'; out[i++] = 't';  break;
            default:
                if (c < 0x20) {
                    i += (size_t)snprintf(out + i, cap - i, "\\u%04x", c);
                } else {
                    out[i++] = (char)c;
                }
        }
    }
    out[i++] = '"';
    out[i]   = '\0';
    return out;
}

/*
 * Build request JSON for a given op, optional key, and optional path.
 * Returns malloc'd string, caller frees. Returns NULL on OOM.
 */
static char *build_request(const char *op, const char *key,
                            const char *path) {
    char *op_e   = json_escape(op);
    char *key_e  = key  ? json_escape(key)  : NULL;
    char *path_e = path ? json_escape(path) : NULL;

    if (!op_e || (key && !key_e) || (path && !path_e)) {
        free(op_e); free(key_e); free(path_e);
        return NULL;
    }

    /* Maximum buffer size: op + key + path + boilerplate */
    size_t cap = 32
                 + (op_e   ? strlen(op_e)   : 0)
                 + (key_e  ? strlen(key_e)  + 10 : 0)
                 + (path_e ? strlen(path_e) + 10 : 0);
    char *buf = (char *)malloc(cap);
    if (!buf) {
        free(op_e); free(key_e); free(path_e);
        return NULL;
    }

    if (key_e && path_e) {
        snprintf(buf, cap, "{\"op\":%s,\"key\":%s,\"path\":%s}",
                 op_e, key_e, path_e);
    } else if (key_e) {
        snprintf(buf, cap, "{\"op\":%s,\"key\":%s}", op_e, key_e);
    } else if (path_e) {
        snprintf(buf, cap, "{\"op\":%s,\"path\":%s}", op_e, path_e);
    } else {
        snprintf(buf, cap, "{\"op\":%s}", op_e);
    }

    free(op_e); free(key_e); free(path_e);
    return buf;
}

/* -------------------------------------------------------------------------
 * Core operations
 * ---------------------------------------------------------------------- */

comb_result_t *comb_get(comb_client_t *client, const char *key,
                        const char *path) {
    if (!key) return result_error("key must not be NULL");

    char *req = build_request("get", key, path);
    if (!req) return result_error("out of memory");

    comb_result_t *r = do_request(client, req);
    free(req);
    return r;
}

int comb_poke(comb_client_t *client, const char *key, const char *path) {
    if (!client || client->fd < 0) return -1;
    if (!key) return -1;

    char *req = build_request("poke", key, path);
    if (!req) return -1;

    comb_result_t *r = do_request(client, req);
    free(req);
    int ok = comb_result_ok(r);
    comb_result_free(r);
    return ok ? 0 : -1;
}

int comb_set_context(comb_client_t *client, const char *path) {
    if (!client || client->fd < 0) return -1;
    if (!path) return -1;

    char *req = build_request("context", NULL, path);
    if (!req) return -1;

    comb_result_t *r = do_request(client, req);
    free(req);
    int ok = comb_result_ok(r);
    comb_result_free(r);
    return ok ? 0 : -1;
}

comb_result_t *comb_list(comb_client_t *client) {
    char *req = build_request("list", NULL, NULL);
    if (!req) return result_error("out of memory");
    comb_result_t *r = do_request(client, req);
    free(req);
    return r;
}

comb_result_t *comb_status(comb_client_t *client) {
    char *req = build_request("status", NULL, NULL);
    if (!req) return result_error("out of memory");
    comb_result_t *r = do_request(client, req);
    free(req);
    return r;
}

/* -------------------------------------------------------------------------
 * Result accessors
 * ---------------------------------------------------------------------- */

int comb_result_ok(const comb_result_t *r) {
    if (!r) return 0;
    return r->ok;
}

int comb_result_is_hit(const comb_result_t *r) {
    if (!r) return 0;
    return r->is_hit;
}

const char *comb_result_error(const comb_result_t *r) {
    if (!r) return NULL;
    return r->error;
}

/*
 * Navigate to the target node for field access.
 *
 * For a hit result:
 *   - If data is a JSON object and field is non-NULL, look up field in it.
 *   - If data is a scalar (string/number/bool), return it directly
 *     regardless of field (single-field query like "git.branch").
 *   - If field is NULL, return the data node directly.
 */
static const json_node_t *resolve_field(const comb_result_t *r,
                                         const char *field) {
    if (!r || !r->is_hit || !r->data) return NULL;

    const json_node_t *data = r->data;

    if (data->type == JSON_OBJECT && field) {
        return json_get(data, field);
    }

    /* Scalar data: ignore field */
    return data;
}

const char *comb_result_get_str(const comb_result_t *r, const char *field) {
    const json_node_t *node = resolve_field(r, field);
    return json_as_str(node);
}

int comb_result_get_int(const comb_result_t *r, const char *field,
                        int64_t *out) {
    if (!out) return 0;
    const json_node_t *node = resolve_field(r, field);
    if (!node) return 0;
    int ok = 0;
    int64_t v = json_as_int(node, &ok);
    if (ok) *out = v;
    return ok;
}

int comb_result_get_float(const comb_result_t *r, const char *field,
                          double *out) {
    if (!out) return 0;
    const json_node_t *node = resolve_field(r, field);
    if (!node) return 0;
    int ok = 0;
    double v = json_as_float(node, &ok);
    if (ok) *out = v;
    return ok;
}

int comb_result_get_bool(const comb_result_t *r, const char *field,
                         int *out) {
    if (!out) return 0;
    const json_node_t *node = resolve_field(r, field);
    if (!node) return 0;
    int ok = 0;
    int v = json_as_bool(node, &ok);
    if (ok) *out = v;
    return ok;
}

uint64_t comb_result_age_ms(const comb_result_t *r) {
    if (!r) return 0;
    return r->age_ms;
}

int comb_result_stale(const comb_result_t *r) {
    if (!r) return 0;
    return r->stale;
}

const char *comb_result_raw_json(const comb_result_t *r) {
    if (!r) return NULL;
    return r->raw_json;
}

void comb_result_free(comb_result_t *r) {
    if (!r) return;
    free(r->error);
    free(r->raw_json);
    json_free(r->parsed);
    free(r);
}
