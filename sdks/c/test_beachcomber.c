/*
 * test_beachcomber.c — unit and integration tests for libbeachcomber
 *
 * Build:  make test
 * Run:    ./test_beachcomber
 *
 * Exit code: 0 = all pass, non-zero = failures.
 */

#include "beachcomber.h"
#include "json.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <errno.h>

/* -------------------------------------------------------------------------
 * Minimal test framework
 * ---------------------------------------------------------------------- */

static int g_pass = 0;
static int g_fail = 0;
static const char *g_suite = "";

#define SUITE(name) do { g_suite = (name); printf("\n--- %s ---\n", g_suite); } while (0)

#define CHECK(expr) do {                                                \
    if (expr) {                                                         \
        g_pass++;                                                        \
    } else {                                                            \
        g_fail++;                                                        \
        fprintf(stderr, "  FAIL [%s:%d] %s\n", __FILE__, __LINE__, #expr); \
    }                                                                   \
} while (0)

#define CHECK_STR_EQ(a, b) do {                                         \
    const char *_a = (a), *_b = (b);                                   \
    if (_a && _b && strcmp(_a, _b) == 0) {                             \
        g_pass++;                                                        \
    } else {                                                            \
        g_fail++;                                                        \
        fprintf(stderr, "  FAIL [%s:%d] %s == %s: \"%s\" != \"%s\"\n", \
                __FILE__, __LINE__, #a, #b,                            \
                _a ? _a : "(null)", _b ? _b : "(null)");               \
    }                                                                   \
} while (0)

/* -------------------------------------------------------------------------
 * JSON parser unit tests
 * ---------------------------------------------------------------------- */

static void test_json_null(void) {
    json_node_t *n = json_parse("null");
    CHECK(n != NULL);
    CHECK(n->type == JSON_NULL);
    json_free(n);
}

static void test_json_booleans(void) {
    json_node_t *t = json_parse("true");
    CHECK(t != NULL && t->type == JSON_BOOL && t->val.boolean == 1);
    json_free(t);

    json_node_t *f = json_parse("false");
    CHECK(f != NULL && f->type == JSON_BOOL && f->val.boolean == 0);
    json_free(f);
}

static void test_json_integers(void) {
    json_node_t *n = json_parse("42");
    CHECK(n != NULL && n->type == JSON_INT && n->val.integer == 42);
    json_free(n);

    n = json_parse("-7");
    CHECK(n != NULL && n->type == JSON_INT && n->val.integer == -7);
    json_free(n);

    n = json_parse("0");
    CHECK(n != NULL && n->type == JSON_INT && n->val.integer == 0);
    json_free(n);
}

static void test_json_floats(void) {
    json_node_t *n = json_parse("3.14");
    CHECK(n != NULL && n->type == JSON_FLOAT);
    CHECK(n->val.floating > 3.13 && n->val.floating < 3.15);
    json_free(n);

    n = json_parse("1e3");
    CHECK(n != NULL && n->type == JSON_FLOAT);
    CHECK(n->val.floating == 1000.0);
    json_free(n);

    n = json_parse("-2.5");
    CHECK(n != NULL && n->type == JSON_FLOAT);
    CHECK(n->val.floating == -2.5);
    json_free(n);
}

static void test_json_strings(void) {
    json_node_t *n = json_parse("\"hello\"");
    CHECK(n != NULL && n->type == JSON_STRING);
    CHECK_STR_EQ(n->val.string, "hello");
    json_free(n);

    /* Empty string */
    n = json_parse("\"\"");
    CHECK(n != NULL && n->type == JSON_STRING);
    CHECK_STR_EQ(n->val.string, "");
    json_free(n);

    /* Escape sequences */
    n = json_parse("\"tab\\there\"");
    CHECK(n != NULL && n->type == JSON_STRING);
    CHECK(strchr(n->val.string, '\t') != NULL);
    json_free(n);

    n = json_parse("\"line\\nbreak\"");
    CHECK(n != NULL && n->type == JSON_STRING);
    CHECK(strchr(n->val.string, '\n') != NULL);
    json_free(n);

    /* Escaped quote and backslash */
    n = json_parse("\"a\\\"b\\\\c\"");
    CHECK(n != NULL && n->type == JSON_STRING);
    CHECK_STR_EQ(n->val.string, "a\"b\\c");
    json_free(n);
}

static void test_json_unicode_escape(void) {
    /* \u0041 = 'A' */
    json_node_t *n = json_parse("\"\\u0041\"");
    CHECK(n != NULL && n->type == JSON_STRING);
    CHECK_STR_EQ(n->val.string, "A");
    json_free(n);
}

static void test_json_objects(void) {
    json_node_t *n = json_parse("{\"ok\":true,\"value\":42}");
    CHECK(n != NULL && n->type == JSON_OBJECT);
    CHECK(n->n_children == 2);

    json_node_t *ok = json_get(n, "ok");
    CHECK(ok != NULL && ok->type == JSON_BOOL && ok->val.boolean == 1);

    json_node_t *val = json_get(n, "value");
    CHECK(val != NULL && val->type == JSON_INT && val->val.integer == 42);

    CHECK(json_get(n, "missing") == NULL);
    json_free(n);
}

static void test_json_empty_object(void) {
    json_node_t *n = json_parse("{}");
    CHECK(n != NULL && n->type == JSON_OBJECT && n->n_children == 0);
    json_free(n);
}

static void test_json_arrays(void) {
    json_node_t *n = json_parse("[1,2,3]");
    CHECK(n != NULL && n->type == JSON_ARRAY && n->n_children == 3);

    json_node_t *c = n->children;
    CHECK(c != NULL && c->type == JSON_INT && c->val.integer == 1);
    c = c->next;
    CHECK(c != NULL && c->type == JSON_INT && c->val.integer == 2);
    c = c->next;
    CHECK(c != NULL && c->type == JSON_INT && c->val.integer == 3);
    CHECK(c->next == NULL);
    json_free(n);
}

static void test_json_empty_array(void) {
    json_node_t *n = json_parse("[]");
    CHECK(n != NULL && n->type == JSON_ARRAY && n->n_children == 0);
    json_free(n);
}

static void test_json_nested(void) {
    const char *src = "{\"ok\":true,\"data\":{\"branch\":\"main\","
                      "\"dirty\":false,\"staged\":2},"
                      "\"age_ms\":120,\"stale\":false}";
    json_node_t *root = json_parse(src);
    CHECK(root != NULL && root->type == JSON_OBJECT);

    json_node_t *data = json_get(root, "data");
    CHECK(data != NULL && data->type == JSON_OBJECT);

    json_node_t *branch = json_get(data, "branch");
    CHECK(branch != NULL && branch->type == JSON_STRING);
    CHECK_STR_EQ(branch->val.string, "main");

    json_node_t *dirty = json_get(data, "dirty");
    CHECK(dirty != NULL && dirty->type == JSON_BOOL && dirty->val.boolean == 0);

    json_node_t *staged = json_get(data, "staged");
    CHECK(staged != NULL && staged->type == JSON_INT && staged->val.integer == 2);

    json_node_t *age = json_get(root, "age_ms");
    CHECK(age != NULL && age->type == JSON_INT && age->val.integer == 120);

    json_free(root);
}

static void test_json_null_data_field(void) {
    /* "data":null means miss */
    json_node_t *n = json_parse("{\"ok\":true,\"data\":null}");
    CHECK(n != NULL);
    json_node_t *data = json_get(n, "data");
    CHECK(data != NULL && data->type == JSON_NULL);
    json_free(n);
}

static void test_json_bad_input(void) {
    CHECK(json_parse(NULL)   == NULL);
    CHECK(json_parse("")     == NULL);
    CHECK(json_parse("{")    == NULL);
    CHECK(json_parse("}")    == NULL);
    CHECK(json_parse("[")    == NULL);
    CHECK(json_parse("\"")   == NULL);   /* unterminated string */
}

static void test_json_whitespace(void) {
    json_node_t *n = json_parse("  {  \"k\"  :  \"v\"  }  ");
    CHECK(n != NULL && n->type == JSON_OBJECT);
    json_node_t *v = json_get(n, "k");
    CHECK(v != NULL);
    CHECK_STR_EQ(v->val.string, "v");
    json_free(n);
}

static void test_json_accessors(void) {
    /* json_as_int on a float whole number */
    json_node_t *f = json_parse("3.0");
    int ok = 0;
    int64_t iv = json_as_int(f, &ok);
    CHECK(ok == 1 && iv == 3);
    json_free(f);

    /* json_as_float on integer */
    json_node_t *i = json_parse("7");
    double dv = json_as_float(i, &ok);
    CHECK(ok == 1 && dv == 7.0);
    json_free(i);

    /* wrong type */
    json_node_t *s = json_parse("\"hello\"");
    json_as_int(s, &ok);
    CHECK(ok == 0);
    json_free(s);
}

/* -------------------------------------------------------------------------
 * Socket path discovery tests
 * ---------------------------------------------------------------------- */

static void test_socket_path_tmpdir(void) {
    /* Save TMPDIR before overriding — subsequent tests rely on it. */
    const char *saved_tmpdir = getenv("TMPDIR");
    char saved_tmpdir_buf[4096] = {0};
    if (saved_tmpdir) strncpy(saved_tmpdir_buf, saved_tmpdir, sizeof(saved_tmpdir_buf) - 1);

    /* Unset XDG_RUNTIME_DIR so we fall through to TMPDIR path */
    unsetenv("XDG_RUNTIME_DIR");
    setenv("TMPDIR", "/tmp/testdir", 1);

    char buf[512];
    char *p = comb_socket_path(buf, sizeof(buf));
    CHECK(p != NULL);

    /* Path must contain "beachcomber-<uid>" */
    CHECK(strstr(buf, "beachcomber-") != NULL);
    CHECK(strstr(buf, "/sock") != NULL);
    /* Must not start with the xdg path */
    CHECK(strncmp(buf, "/tmp/testdir/beachcomber-", 25) == 0);

    /* Restore TMPDIR. */
    if (saved_tmpdir_buf[0]) setenv("TMPDIR", saved_tmpdir_buf, 1);
    else unsetenv("TMPDIR");
}

static void test_socket_path_xdg_nonexistent(void) {
    /* Save TMPDIR before overriding — subsequent tests rely on it. */
    const char *saved_tmpdir = getenv("TMPDIR");
    char saved_tmpdir_buf[4096] = {0};
    if (saved_tmpdir) strncpy(saved_tmpdir_buf, saved_tmpdir, sizeof(saved_tmpdir_buf) - 1);

    /* Set XDG to a path that will have no running socket */
    setenv("XDG_RUNTIME_DIR", "/nonexistent_xdg_8f3a2b", 1);
    setenv("TMPDIR", "/tmp/fallback99", 1);

    char buf[512];
    char *p = comb_socket_path(buf, sizeof(buf));
    CHECK(p != NULL);
    /* Should have fallen back to TMPDIR */
    CHECK(strncmp(buf, "/tmp/fallback99/beachcomber-", 28) == 0);

    unsetenv("XDG_RUNTIME_DIR");
    /* Restore TMPDIR. */
    if (saved_tmpdir_buf[0]) setenv("TMPDIR", saved_tmpdir_buf, 1);
    else unsetenv("TMPDIR");
}

static void test_socket_path_too_small(void) {
    char tiny[4];
    char *p = comb_socket_path(tiny, sizeof(tiny));
    CHECK(p == NULL);
}

static void test_socket_path_no_tmpdir(void) {
    /* Save TMPDIR before unsetting — subsequent tests rely on it. */
    const char *saved_tmpdir = getenv("TMPDIR");
    char saved_tmpdir_buf[4096] = {0};
    if (saved_tmpdir) strncpy(saved_tmpdir_buf, saved_tmpdir, sizeof(saved_tmpdir_buf) - 1);

    unsetenv("XDG_RUNTIME_DIR");
    unsetenv("TMPDIR");

    char buf[512];
    char *p = comb_socket_path(buf, sizeof(buf));
    CHECK(p != NULL);
    /* Should fall back to /tmp */
    CHECK(strncmp(buf, "/tmp/beachcomber-", 17) == 0);

    /* Restore TMPDIR so subsequent tests that create sockets are not affected. */
    if (saved_tmpdir_buf[0]) setenv("TMPDIR", saved_tmpdir_buf, 1);
}

/* -------------------------------------------------------------------------
 * Result accessor unit tests (no network — build results from raw JSON)
 * ---------------------------------------------------------------------- */

/*
 * Expose the internal parse_response entry point so tests can use it
 * without a live socket.  We replicate the same call path used in
 * do_request() by using comb_get with a mock server (below), but for
 * pure unit tests we drive comb_result accessors directly via a thin
 * helper that duplicates the parse path.
 *
 * Rather than coupling to private internals, we use the mock server path
 * for integration tests and only test the public API here.
 */

/* Build a fake comb_result_t by going through a live round-trip over a
 * socketpair.  This is a local helper used only in this test file.
 * It sends `response_json` as if it were the daemon responding, then
 * calls comb_get() on the client end to exercise the full parse path. */

typedef struct {
    int server_fd;
    const char *response_json;
} mock_server_args_t;

static void *mock_server_thread(void *arg) {
    mock_server_args_t *args = (mock_server_args_t *)arg;

    /* Accept one connection */
    int conn_fd = accept(args->server_fd, NULL, NULL);
    if (conn_fd < 0) return NULL;

    /* Drain the request line byte-by-byte until '\n'. */
    char drain_ch;
    ssize_t drain_n;
    while ((drain_n = read(conn_fd, &drain_ch, 1)) > 0 && drain_ch != '\n') { /* drain */ }

    /* Send canned response */
    const char *resp = args->response_json;
    size_t len = strlen(resp);
    write(conn_fd, resp, len);
    if (resp[len - 1] != '\n') write(conn_fd, "\n", 1);

    close(conn_fd);
    return NULL;
}

/*
 * Spin up a Unix-socket mock server in a thread, connect a client to it,
 * issue one comb_get("k", NULL), return the result.
 */
static comb_result_t *get_with_mock(const char *response_json) {
    /* Create a temp socket path */
    char sock_path[256];
    const char *tmpdir = getenv("TMPDIR");
    if (!tmpdir || !*tmpdir) tmpdir = "/tmp";
    /* Trim trailing slash if present */
    char tmpdir_buf[256];
    strncpy(tmpdir_buf, tmpdir, sizeof(tmpdir_buf) - 1);
    tmpdir_buf[sizeof(tmpdir_buf) - 1] = '\0';
    size_t tlen = strlen(tmpdir_buf);
    if (tlen > 0 && tmpdir_buf[tlen - 1] == '/') tmpdir_buf[tlen - 1] = '\0';
    snprintf(sock_path, sizeof(sock_path),
             "%s/comb_test_%d_%d.sock", tmpdir_buf, (int)getpid(), rand());

    /* Set up listening socket */
    int srv_fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (srv_fd < 0) return NULL;

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, sock_path, sizeof(addr.sun_path) - 1);
    unlink(sock_path);

    if (bind(srv_fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(srv_fd);
        return NULL;
    }
    if (listen(srv_fd, 1) != 0) {
        close(srv_fd);
        unlink(sock_path);
        return NULL;
    }

    mock_server_args_t args = { srv_fd, response_json };
    pthread_t tid;
    pthread_create(&tid, NULL, mock_server_thread, &args);

    /* Connect client */
    comb_client_t *client = comb_connect_path(sock_path);
    comb_result_t *result = NULL;
    if (client) {
        result = comb_get(client, "k", NULL);
        comb_disconnect(client);
    }

    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);

    return result;
}

/* Same as get_with_mock but uses mock server that handles multiple requests,
 * for testing refresh / context / status. */
typedef struct {
    int server_fd;
    const char **responses;   /* NULL-terminated array */
} multi_mock_args_t;

static void *multi_mock_server_thread(void *arg) {
    multi_mock_args_t *args = (multi_mock_args_t *)arg;
    int conn_fd = accept(args->server_fd, NULL, NULL);
    if (conn_fd < 0) return NULL;

    for (int i = 0; args->responses[i] != NULL; i++) {
        /* Drain the request line byte-by-byte until '\n' to avoid consuming
         * bytes that belong to the next request. */
        char ch;
        ssize_t n;
        while ((n = read(conn_fd, &ch, 1)) > 0 && ch != '\n') { /* drain */ }
        if (n <= 0) break;

        const char *resp = args->responses[i];
        size_t len = strlen(resp);
        write(conn_fd, resp, len);
        if (resp[len - 1] != '\n') write(conn_fd, "\n", 1);
    }

    close(conn_fd);
    return NULL;
}

static comb_client_t *start_multi_mock(const char **responses,
                                        int *srv_fd_out,
                                        char *sock_path_out,
                                        pthread_t *tid_out,
                                        multi_mock_args_t *args_out) {
    const char *tmpdir2 = getenv("TMPDIR");
    if (!tmpdir2 || !*tmpdir2) tmpdir2 = "/tmp";
    char tmpdir_buf2[256];
    strncpy(tmpdir_buf2, tmpdir2, sizeof(tmpdir_buf2) - 1);
    tmpdir_buf2[sizeof(tmpdir_buf2) - 1] = '\0';
    size_t tlen2 = strlen(tmpdir_buf2);
    if (tlen2 > 0 && tmpdir_buf2[tlen2 - 1] == '/') tmpdir_buf2[tlen2 - 1] = '\0';
    snprintf(sock_path_out, 256,
             "%s/comb_multi_%d_%d.sock", tmpdir_buf2, (int)getpid(), rand());

    int srv_fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (srv_fd < 0) return NULL;

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, sock_path_out, sizeof(addr.sun_path) - 1);
    unlink(sock_path_out);

    if (bind(srv_fd, (struct sockaddr *)&addr, sizeof(addr)) != 0 ||
        listen(srv_fd, 1) != 0) {
        close(srv_fd);
        return NULL;
    }

    *srv_fd_out = srv_fd;
    args_out->server_fd = srv_fd;
    args_out->responses = responses;
    pthread_create(tid_out, NULL, multi_mock_server_thread, args_out);

    return comb_connect_path(sock_path_out);
}

/* -------------------------------------------------------------------------
 * Result accessor tests
 * ---------------------------------------------------------------------- */

static void test_result_hit_scalar(void) {
    comb_result_t *r = get_with_mock(
        "{\"ok\":true,\"data\":\"main\",\"age_ms\":120,\"stale\":false}");
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 1);
    CHECK(comb_result_is_hit(r) == 1);
    CHECK(comb_result_error(r) == NULL);
    CHECK_STR_EQ(comb_result_get_str(r, NULL), "main");
    /* field name for scalar is also valid */
    CHECK_STR_EQ(comb_result_get_str(r, "branch"), "main");
    CHECK(comb_result_age_ms(r) == 120);
    CHECK(comb_result_stale(r) == 0);
    comb_result_free(r);
}

static void test_result_hit_object(void) {
    comb_result_t *r = get_with_mock(
        "{\"ok\":true,"
        "\"data\":{\"branch\":\"feat\",\"dirty\":true,\"staged\":3},"
        "\"age_ms\":50,\"stale\":true}");
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 1);
    CHECK(comb_result_is_hit(r) == 1);
    CHECK_STR_EQ(comb_result_get_str(r, "branch"), "feat");

    int bval = 0;
    CHECK(comb_result_get_bool(r, "dirty", &bval) == 1 && bval == 1);

    int64_t ival = 0;
    CHECK(comb_result_get_int(r, "staged", &ival) == 1 && ival == 3);

    CHECK(comb_result_age_ms(r) == 50);
    CHECK(comb_result_stale(r) == 1);
    comb_result_free(r);
}

static void test_result_miss(void) {
    comb_result_t *r = get_with_mock("{\"ok\":true}");
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 1);
    CHECK(comb_result_is_hit(r) == 0);
    CHECK(comb_result_age_ms(r) == 0);
    comb_result_free(r);
}

static void test_result_miss_null_data(void) {
    comb_result_t *r = get_with_mock("{\"ok\":true,\"data\":null}");
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 1);
    CHECK(comb_result_is_hit(r) == 0);
    comb_result_free(r);
}

static void test_result_error_response(void) {
    comb_result_t *r = get_with_mock(
        "{\"ok\":false,\"error\":\"unknown provider: foo\"}");
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 0);
    CHECK(comb_result_is_hit(r) == 0);
    const char *err = comb_result_error(r);
    CHECK(err != NULL && strstr(err, "unknown provider") != NULL);
    comb_result_free(r);
}

static void test_result_raw_json(void) {
    const char *resp = "{\"ok\":true,\"data\":\"v1\",\"age_ms\":0,\"stale\":false}";
    comb_result_t *r = get_with_mock(resp);
    CHECK(r != NULL);
    const char *raw = comb_result_raw_json(r);
    CHECK(raw != NULL);
    CHECK(strstr(raw, "\"ok\"") != NULL);
    comb_result_free(r);
}

static void test_result_int_field(void) {
    comb_result_t *r = get_with_mock(
        "{\"ok\":true,\"data\":{\"count\":99}}");
    int64_t v = 0;
    CHECK(comb_result_get_int(r, "count", &v) == 1);
    CHECK(v == 99);
    comb_result_free(r);
}

static void test_result_float_field(void) {
    comb_result_t *r = get_with_mock(
        "{\"ok\":true,\"data\":{\"score\":4.5}}");
    double d = 0.0;
    CHECK(comb_result_get_float(r, "score", &d) == 1);
    CHECK(d > 4.49 && d < 4.51);
    comb_result_free(r);
}

static void test_result_missing_field(void) {
    comb_result_t *r = get_with_mock(
        "{\"ok\":true,\"data\":{\"a\":\"x\"}}");
    CHECK(comb_result_get_str(r, "z") == NULL);
    int64_t v = 0;
    CHECK(comb_result_get_int(r, "z", &v) == 0);
    comb_result_free(r);
}

static void test_result_null_args(void) {
    /* All accessors on NULL result must not crash */
    CHECK(comb_result_ok(NULL)       == 0);
    CHECK(comb_result_is_hit(NULL)   == 0);
    CHECK(comb_result_error(NULL)    == NULL);
    CHECK(comb_result_get_str(NULL, NULL) == NULL);
    CHECK(comb_result_age_ms(NULL)   == 0);
    CHECK(comb_result_stale(NULL)    == 0);
    CHECK(comb_result_raw_json(NULL) == NULL);
    comb_result_free(NULL); /* must not crash */
}

/* -------------------------------------------------------------------------
 * Integration tests: refresh, context, list, status
 * ---------------------------------------------------------------------- */

static void test_refresh(void) {
    const char *responses[] = { "{\"ok\":true}", NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;

    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    int rc = comb_refresh(c, "git", "/some/path");
    CHECK(rc == 0);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_set_context(void) {
    const char *responses[] = { "{\"ok\":true}", NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;

    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    int rc = comb_set_context(c, "/my/repo");
    CHECK(rc == 0);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_status(void) {
    const char *resp =
        "{\"ok\":true,\"data\":{\"queued\":0,\"running\":1,\"cache_entries\":42}}";
    comb_result_t *r = get_with_mock(resp);
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 1);

    int64_t entries = 0;
    CHECK(comb_result_get_int(r, "cache_entries", &entries) == 1);
    CHECK(entries == 42);
    comb_result_free(r);
}

/* -------------------------------------------------------------------------
 * Integration test: connect to a non-existent socket
 * ---------------------------------------------------------------------- */

static void test_connect_fail(void) {
    comb_client_t *c = comb_connect_path("/tmp/comb_nonexistent_socket_9x.sock");
    CHECK(c == NULL);
}

static void test_refresh_on_null_client(void) {
    CHECK(comb_refresh(NULL, "git", NULL) == -1);
    CHECK(comb_set_context(NULL, "/x") == -1);
}

/* -------------------------------------------------------------------------
 * Integration test: sequential requests on one connection
 * ---------------------------------------------------------------------- */

static void test_multiple_requests(void) {
    const char *responses[] = {
        "{\"ok\":true,\"data\":\"main\",\"age_ms\":10,\"stale\":false}",
        "{\"ok\":true,\"data\":\"host1\",\"age_ms\":5,\"stale\":false}",
        NULL
    };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;

    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_result_t *r1 = comb_get(c, "git.branch", "/repo");
    CHECK(comb_result_ok(r1) == 1);
    CHECK_STR_EQ(comb_result_get_str(r1, NULL), "main");
    comb_result_free(r1);

    comb_result_t *r2 = comb_get(c, "hostname.short", NULL);
    CHECK(comb_result_ok(r2) == 1);
    CHECK_STR_EQ(comb_result_get_str(r2, NULL), "host1");
    comb_result_free(r2);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

/* -------------------------------------------------------------------------
 * New API tests: comb_hello
 * ---------------------------------------------------------------------- */

static void test_hello(void) {
    const char *resp =
        "{\"ok\":true,\"data\":"
        "{\"protocol_version\":\"1\",\"daemon_version\":\"0.4.0\"}}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;

    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_hello_info_t info;
    memset(&info, 0, sizeof(info));
    int rc = comb_hello(c, &info);
    CHECK(rc == 0);
    CHECK_STR_EQ(info.protocol_version, "1");
    CHECK_STR_EQ(info.daemon_version, "0.4.0");

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_hello_null_args(void) {
    /* NULL client and NULL out both return -1 without crashing */
    CHECK(comb_hello(NULL, NULL) == -1);
}

/* -------------------------------------------------------------------------
 * New API tests: comb_get_with_flags
 * ---------------------------------------------------------------------- */

static void test_get_with_flags_no_flags(void) {
    /* force=0, wait=0 — behaves like a plain get */
    const char *resp =
        "{\"ok\":true,\"data\":\"main\",\"age_ms\":10,\"stale\":false}";
    comb_result_t *r = get_with_mock(resp);
    /* get_with_mock issues comb_get; this test exercises the same path via
     * get_with_flags using a dedicated mock. */
    comb_result_free(r);

    const char *responses[] = {
        "{\"ok\":true,\"data\":\"dev\",\"age_ms\":5,\"stale\":false}",
        NULL
    };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t margs;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &margs);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_result_t *r2 = comb_get_with_flags(c, "git.branch", NULL, 0, 0);
    CHECK(comb_result_ok(r2) == 1);
    CHECK(comb_result_is_hit(r2) == 1);
    CHECK_STR_EQ(comb_result_get_str(r2, NULL), "dev");
    comb_result_free(r2);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_get_with_flags_force(void) {
    const char *responses[] = {
        "{\"ok\":true,\"data\":\"fresh\",\"age_ms\":1,\"stale\":false}",
        NULL
    };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t margs;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &margs);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_result_t *r = comb_get_with_flags(c, "git.branch", "/repo", 1, 0);
    CHECK(comb_result_ok(r) == 1);
    CHECK_STR_EQ(comb_result_get_str(r, NULL), "fresh");
    comb_result_free(r);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_get_with_flags_null_key(void) {
    comb_result_t *r = comb_get_with_flags(NULL, NULL, NULL, 0, 0);
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 0);
    comb_result_free(r);
}

/* -------------------------------------------------------------------------
 * New API tests: comb_put / comb_put_null
 * ---------------------------------------------------------------------- */

static void test_put(void) {
    const char *responses[] = { "{\"ok\":true}", NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    int rc = comb_put(c, "mykey", "{\"val\":1}", NULL, NULL);
    CHECK(rc == 0);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_put_error_response(void) {
    const char *responses[] = {
        "{\"ok\":false,\"error\":\"must be a JSON object\"}",
        NULL
    };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    int rc = comb_put(c, "mykey", "\"not-an-object\"", NULL, NULL);
    CHECK(rc == -1);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_put_null(void) {
    const char *responses[] = { "{\"ok\":true}", NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    int rc = comb_put_null(c, "mykey", NULL);
    CHECK(rc == 0);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_put_null_args(void) {
    CHECK(comb_put(NULL, "k", "{}", NULL, NULL) == -1);
    CHECK(comb_put_null(NULL, "k", NULL) == -1);
}

/* -------------------------------------------------------------------------
 * New API tests: comb_introspect
 * ---------------------------------------------------------------------- */

static void test_introspect_raw(void) {
    const char *resp =
        "{\"ok\":true,\"data\":{\"pid\":1234,\"version\":\"0.4.0\"}}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_result_t *r = comb_introspect(c, COMB_INTROSPECT_DAEMON, 0);
    CHECK(r != NULL);
    CHECK(comb_result_ok(r) == 1);
    CHECK(comb_result_is_hit(r) == 1);

    int64_t pid = 0;
    CHECK(comb_result_get_int(r, "pid", &pid) == 1);
    CHECK(pid == 1234);
    comb_result_free(r);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_introspect_daemon_typed(void) {
    const char *resp =
        "{\"ok\":true,\"data\":{"
        "\"pid\":5678,\"version\":\"0.4.0\","
        "\"uptime_secs\":120,\"socket_path\":\"/tmp/comb.sock\","
        "\"config_path\":\"\","
        "\"requests_total\":99,\"in_flight\":2,"
        "\"active_watchers\":3,\"cache_entries\":10}}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_daemon_health_t h;
    int rc = comb_introspect_daemon(c, &h);
    CHECK(rc == 0);
    CHECK(h.pid == 5678);
    CHECK_STR_EQ(h.version, "0.4.0");
    CHECK(h.uptime_secs == 120);
    CHECK_STR_EQ(h.socket_path, "/tmp/comb.sock");
    CHECK_STR_EQ(h.config_path, "");
    CHECK(h.requests_total == 99);
    CHECK(h.in_flight == 2);
    CHECK(h.active_watchers == 3);
    CHECK(h.cache_entries == 10);

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_introspect_daemon_null_args(void) {
    CHECK(comb_introspect_daemon(NULL, NULL) == -1);
}

/* -------------------------------------------------------------------------
 * New API tests: comb_status_rows
 * ---------------------------------------------------------------------- */

static void test_status_rows(void) {
    const char *resp =
        "{\"ok\":true,\"data\":["
        "{\"provider\":\"git\",\"field\":\"branch\","
        "\"path\":\"/repo\",\"value\":\"main\","
        "\"age_ms\":50,\"stale\":false},"
        "{\"provider\":\"hostname\",\"field\":\"\","
        "\"path\":\"\",\"value\":\"myhost\","
        "\"age_ms\":100,\"stale\":true}"
        "]}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_cache_row_t *rows = NULL;
    size_t n = 0;
    int rc = comb_status_rows(c, &rows, &n);
    CHECK(rc == 0);
    CHECK(n == 2);

    if (n >= 2 && rows) {
        CHECK_STR_EQ(rows[0].provider, "git");
        CHECK_STR_EQ(rows[0].field, "branch");
        CHECK_STR_EQ(rows[0].path, "/repo");
        CHECK(rows[0].age_ms == 50);
        CHECK(rows[0].stale == 0);

        CHECK_STR_EQ(rows[1].provider, "hostname");
        CHECK(rows[1].age_ms == 100);
        CHECK(rows[1].stale == 1);
    }

    comb_free_cache_rows(rows, n);
    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_status_rows_two(void) {
    /* Verify both rows returned when daemon sends 2 */
    const char *resp =
        "{\"ok\":true,\"data\":["
        "{\"provider\":\"a\",\"field\":\"\",\"path\":\"\","
        "\"value\":\"x\",\"age_ms\":1,\"stale\":false},"
        "{\"provider\":\"b\",\"field\":\"\",\"path\":\"\","
        "\"value\":\"y\",\"age_ms\":2,\"stale\":false}"
        "]}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_cache_row_t *rows = NULL;
    size_t n = 0;
    int rc = comb_status_rows(c, &rows, &n);
    CHECK(rc == 0);
    CHECK(n == 2);
    if (rows && n >= 1) CHECK_STR_EQ(rows[0].provider, "a");
    if (rows && n >= 2) CHECK_STR_EQ(rows[1].provider, "b");

    comb_free_cache_rows(rows, n);
    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_status_rows_null_args(void) {
    CHECK(comb_status_rows(NULL, NULL, NULL) == -1);
}

static void test_status_row_exposes_lifecycle_fields(void) {
    /* Wire format: kind is an object with kind/decay/watches_files siblings;
     * poll_interval_secs, keep_alive_polls, fsevents_reinstate are row-level. */
    const char *resp =
        "{\"ok\":true,\"data\":["
        "{\"provider\":\"git\",\"field\":\"branch\",\"path\":\"/repo\","
        "\"value\":\"main\",\"age_ms\":10,\"stale\":false,"
        "\"kind\":{\"kind\":\"lifecycle\",\"decay\":0,\"watches_files\":true},"
        "\"poll_interval_secs\":5,\"keep_alive_polls\":3,"
        "\"fsevents_reinstate\":false}"
        "]}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_cache_row_t *rows = NULL;
    size_t n = 0;
    int rc = comb_status_rows(c, &rows, &n);
    CHECK(rc == 0);
    CHECK(n == 1);

    if (rows && n >= 1) {
        CHECK_STR_EQ(rows[0].provider, "git");
        CHECK(rows[0].kind != NULL);
        CHECK_STR_EQ(rows[0].kind, "lifecycle");
        CHECK(rows[0].has_lifecycle == 1);
        CHECK(rows[0].poll_interval_secs > 0);
        CHECK(rows[0].keep_alive_polls > 0);
        CHECK(rows[0].decay == 0);
        CHECK(rows[0].watches_files == 1);
        CHECK(rows[0].in_failure == 0);
        CHECK(rows[0].failure_suppressed_until_unix_ms == -1);
    }

    comb_free_cache_rows(rows, n);
    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_status_row_non_lifecycle_kind(void) {
    /* A "once" kind row should have kind set but has_lifecycle false */
    const char *resp =
        "{\"ok\":true,\"data\":["
        "{\"provider\":\"hostname\",\"field\":\"name\",\"path\":\"\","
        "\"value\":\"myhost\",\"age_ms\":5,\"stale\":false,"
        "\"kind\":{\"kind\":\"once\"}}"
        "]}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_cache_row_t *rows = NULL;
    size_t n = 0;
    int rc = comb_status_rows(c, &rows, &n);
    CHECK(rc == 0);
    CHECK(n == 1);

    if (rows && n >= 1) {
        CHECK(rows[0].kind != NULL);
        CHECK_STR_EQ(rows[0].kind, "once");
        CHECK(rows[0].has_lifecycle == 0);
        CHECK(rows[0].decay == -1);
        CHECK(rows[0].poll_interval_secs == 0);
    }

    comb_free_cache_rows(rows, n);
    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_status_row_failure_fields(void) {
    /* Row with failure object — in_failure should be true */
    const char *resp =
        "{\"ok\":true,\"data\":["
        "{\"provider\":\"git\",\"field\":\"branch\",\"path\":\"/repo\","
        "\"value\":\"main\",\"age_ms\":10,\"stale\":true,"
        "\"kind\":{\"kind\":\"lifecycle\",\"decay\":2,\"watches_files\":false},"
        "\"poll_interval_secs\":30,\"keep_alive_polls\":5,"
        "\"fsevents_reinstate\":false,"
        "\"failure\":{\"consecutive_failures\":3,\"suppressed_until_unix_ms\":9999}}"
        "]}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_cache_row_t *rows = NULL;
    size_t n = 0;
    int rc = comb_status_rows(c, &rows, &n);
    CHECK(rc == 0);
    CHECK(n == 1);

    if (rows && n >= 1) {
        CHECK(rows[0].in_failure == 1);
        CHECK(rows[0].failure_consecutive_failures == 3);
        CHECK(rows[0].failure_suppressed_until_unix_ms == 9999);
    }

    comb_free_cache_rows(rows, n);
    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_status_rows_free_null(void) {
    /* Must not crash */
    comb_free_cache_rows(NULL, 0);
}

/* -------------------------------------------------------------------------
 * New API tests: comb_watch
 * ---------------------------------------------------------------------- */

/* Watch mock server: sends the watch response on the same connection the
 * client opens for the watch stream. We use the multi_mock infrastructure
 * but the watch handle opens its own fd to the listening socket. */
static void test_watch_next(void) {
    const char *resp =
        "{\"ok\":true,\"data\":7,\"age_ms\":30,\"stale\":false}";
    const char *responses[] = { resp, NULL };
    char sock_path[256];
    int srv_fd;
    pthread_t tid;
    multi_mock_args_t args;

    /* We use start_multi_mock but connect the client ourselves so the
     * watch handle can open its own connection to the same srv_fd. */
    comb_client_t *c = start_multi_mock(responses, &srv_fd, sock_path,
                                         &tid, &args);
    CHECK(c != NULL);
    if (!c) goto done;

    comb_watch_handle_t *wh = comb_watch(c, "fixture_w.count", NULL);
    CHECK(wh != NULL);

    if (wh) {
        comb_watch_event_t ev;
        memset(&ev, 0, sizeof(ev));
        int ret = comb_watch_next(wh, &ev, 2000);
        CHECK(ret == 1);
        CHECK_STR_EQ(ev.data_json, "7");
        CHECK(ev.age_ms == 30);
        CHECK(ev.stale == 0);
        comb_watch_free(wh);
    }

    comb_disconnect(c);
done:
    pthread_join(tid, NULL);
    close(srv_fd);
    unlink(sock_path);
}

static void test_watch_free_null(void) {
    /* Must not crash */
    comb_watch_free(NULL);
}

static void test_watch_null_args(void) {
    CHECK(comb_watch(NULL, NULL, NULL) == NULL);
    CHECK(comb_watch_next(NULL, NULL, 0) == -1);
}

/* -------------------------------------------------------------------------
 * Main
 * ---------------------------------------------------------------------- */

int main(void) {
    srand((unsigned)getpid());

    SUITE("JSON parser — primitives");
    test_json_null();
    test_json_booleans();
    test_json_integers();
    test_json_floats();
    test_json_strings();
    test_json_unicode_escape();

    SUITE("JSON parser — structures");
    test_json_objects();
    test_json_empty_object();
    test_json_arrays();
    test_json_empty_array();
    test_json_nested();
    test_json_null_data_field();
    test_json_bad_input();
    test_json_whitespace();

    SUITE("JSON accessors");
    test_json_accessors();

    SUITE("Socket path discovery");
    test_socket_path_tmpdir();
    test_socket_path_xdg_nonexistent();
    test_socket_path_too_small();
    test_socket_path_no_tmpdir();

    SUITE("Result accessors — scalar hit");
    test_result_hit_scalar();

    SUITE("Result accessors — object hit");
    test_result_hit_object();

    SUITE("Result accessors — miss");
    test_result_miss();
    test_result_miss_null_data();

    SUITE("Result accessors — error");
    test_result_error_response();

    SUITE("Result accessors — misc");
    test_result_raw_json();
    test_result_int_field();
    test_result_float_field();
    test_result_missing_field();
    test_result_null_args();

    SUITE("Integration — mock server");
    test_refresh();
    test_set_context();
    test_status();
    test_connect_fail();
    test_refresh_on_null_client();
    test_multiple_requests();

    SUITE("New API — comb_hello");
    test_hello();
    test_hello_null_args();

    SUITE("New API — comb_get_with_flags");
    test_get_with_flags_no_flags();
    test_get_with_flags_force();
    test_get_with_flags_null_key();

    SUITE("New API — comb_put / comb_put_null");
    test_put();
    test_put_error_response();
    test_put_null();
    test_put_null_args();

    SUITE("New API — comb_introspect");
    test_introspect_raw();
    test_introspect_daemon_typed();
    test_introspect_daemon_null_args();

    SUITE("New API — comb_status_rows");
    test_status_rows();
    test_status_rows_two();
    test_status_rows_null_args();
    test_status_row_exposes_lifecycle_fields();
    test_status_row_non_lifecycle_kind();
    test_status_row_failure_fields();
    test_status_rows_free_null();

    SUITE("New API — comb_watch");
    test_watch_next();
    test_watch_free_null();
    test_watch_null_args();

    printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
    return g_fail == 0 ? 0 : 1;
}
