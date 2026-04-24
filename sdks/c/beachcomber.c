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
#include <poll.h>
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

struct comb_watch_handle {
    int fd;               /* Dedicated socket fd for the watch stream */
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

/*
 * Build a get request with optional force/wait flags.
 * Returns malloc'd string, caller frees. Returns NULL on OOM.
 */
static char *build_get_request(const char *key, const char *path,
                                int force, int wait) {
    char *key_e  = json_escape(key);
    char *path_e = path ? json_escape(path) : NULL;

    if (!key_e || (path && !path_e)) {
        free(key_e); free(path_e);
        return NULL;
    }

    /* Build flags fragment */
    char flags[64] = "";
    if (force) {
        size_t fl = strlen(flags);
        snprintf(flags + fl, sizeof(flags) - fl, ",\"force\":true");
    }
    if (wait) {
        size_t fl = strlen(flags);
        snprintf(flags + fl, sizeof(flags) - fl, ",\"wait\":true");
    }

    size_t cap = 64
                 + strlen(key_e)
                 + (path_e ? strlen(path_e) + 10 : 0)
                 + strlen(flags);
    char *buf = (char *)malloc(cap);
    if (!buf) {
        free(key_e); free(path_e);
        return NULL;
    }

    if (path_e) {
        snprintf(buf, cap,
                 "{\"op\":\"get\",\"key\":%s,\"path\":%s%s}",
                 key_e, path_e, flags);
    } else {
        snprintf(buf, cap,
                 "{\"op\":\"get\",\"key\":%s%s}",
                 key_e, flags);
    }

    free(key_e); free(path_e);
    return buf;
}

/*
 * Build a put request. data_json is inserted verbatim (caller ensures valid
 * JSON). ttl and path are optional.
 * Returns malloc'd string, caller frees. Returns NULL on OOM.
 */
static char *build_put_request(const char *key, const char *data_json,
                                const char *ttl, const char *path) {
    char *key_e  = json_escape(key);
    char *path_e = path ? json_escape(path) : NULL;
    char *ttl_e  = ttl  ? json_escape(ttl)  : NULL;

    if (!key_e || (path && !path_e) || (ttl && !ttl_e)) {
        free(key_e); free(path_e); free(ttl_e);
        return NULL;
    }

    size_t data_len  = data_json ? strlen(data_json) : 0;
    size_t path_frag = path_e ? strlen(path_e) + 10 : 0;
    size_t ttl_frag  = ttl_e  ? strlen(ttl_e)  + 10 : 0;
    size_t cap = 64 + strlen(key_e) + data_len + path_frag + ttl_frag;

    char *buf = (char *)malloc(cap);
    if (!buf) {
        free(key_e); free(path_e); free(ttl_e);
        return NULL;
    }

    /* Start building */
    size_t pos = 0;
    pos += (size_t)snprintf(buf + pos, cap - pos,
                             "{\"op\":\"put\",\"key\":%s", key_e);

    if (data_json) {
        pos += (size_t)snprintf(buf + pos, cap - pos,
                                 ",\"data\":%s", data_json);
    }
    if (ttl_e) {
        pos += (size_t)snprintf(buf + pos, cap - pos,
                                 ",\"ttl\":%s", ttl_e);
    }
    if (path_e) {
        pos += (size_t)snprintf(buf + pos, cap - pos,
                                 ",\"path\":%s", path_e);
    }
    snprintf(buf + pos, cap - pos, "}");

    free(key_e); free(path_e); free(ttl_e);
    return buf;
}

/* Map comb_introspect_subject_t to its wire string. */
static const char *introspect_subject_str(comb_introspect_subject_t subject) {
    switch (subject) {
        case COMB_INTROSPECT_DAEMON:    return "daemon";
        case COMB_INTROSPECT_PROVIDERS: return "providers";
        case COMB_INTROSPECT_CONFIG:    return "config";
        case COMB_INTROSPECT_CACHE:     return "cache";
        case COMB_INTROSPECT_LIFECYCLE: return "lifecycle";
        case COMB_INTROSPECT_WATCHES:   return "watches";
        case COMB_INTROSPECT_TIMERS:    return "timers";
        case COMB_INTROSPECT_DEMAND:    return "demand";
        case COMB_INTROSPECT_PROCS:     return "procs";
    }
    return "daemon";
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

comb_result_t *comb_get_with_flags(comb_client_t *client, const char *key,
                                    const char *path, int force, int wait) {
    if (!key) return result_error("key must not be NULL");

    char *req = build_get_request(key, path, force, wait);
    if (!req) return result_error("out of memory");

    comb_result_t *r = do_request(client, req);
    free(req);
    return r;
}

int comb_refresh(comb_client_t *client, const char *key, const char *path) {
    if (!client || client->fd < 0) return -1;
    if (!key) return -1;

    char *req = build_request("refresh", key, path);
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

comb_result_t *comb_status(comb_client_t *client) {
    char *req = build_request("status", NULL, NULL);
    if (!req) return result_error("out of memory");
    comb_result_t *r = do_request(client, req);
    free(req);
    return r;
}

int comb_hello(comb_client_t *client, comb_hello_info_t *out) {
    if (!client || client->fd < 0) return -1;
    if (!out) return -1;

    char *req = build_request("hello", NULL, NULL);
    if (!req) return -1;

    comb_result_t *r = do_request(client, req);
    free(req);

    if (!comb_result_ok(r) || !r->data) {
        comb_result_free(r);
        return -1;
    }

    memset(out, 0, sizeof(*out));

    json_node_t *pv = json_get(r->data, "protocol_version");
    const char *pvs = json_as_str(pv);
    if (pvs) safe_strcpy(out->protocol_version, sizeof(out->protocol_version), pvs);

    json_node_t *dv = json_get(r->data, "daemon_version");
    const char *dvs = json_as_str(dv);
    if (dvs) safe_strcpy(out->daemon_version, sizeof(out->daemon_version), dvs);

    comb_result_free(r);
    return 0;
}

int comb_put(comb_client_t *client, const char *key, const char *data_json,
             const char *ttl, const char *path) {
    if (!client || client->fd < 0) return -1;
    if (!key) return -1;

    char *req = build_put_request(key, data_json, ttl, path);
    if (!req) return -1;

    comb_result_t *r = do_request(client, req);
    free(req);
    int ok = comb_result_ok(r);
    comb_result_free(r);
    return ok ? 0 : -1;
}

int comb_put_null(comb_client_t *client, const char *key, const char *path) {
    return comb_put(client, key, NULL, NULL, path);
}

int comb_introspect_daemon(comb_client_t *client, comb_daemon_health_t *out) {
    if (!client || client->fd < 0) return -1;
    if (!out) return -1;

    comb_result_t *r = comb_introspect(client, COMB_INTROSPECT_DAEMON, 0);
    if (!comb_result_ok(r) || !r->data) {
        comb_result_free(r);
        return -1;
    }

    memset(out, 0, sizeof(*out));

    json_node_t *pid_node = json_get(r->data, "pid");
    if (pid_node) {
        int ok2 = 0;
        int64_t v = json_as_int(pid_node, &ok2);
        if (ok2) out->pid = v;
    }

    json_node_t *ver_node = json_get(r->data, "version");
    const char *vs = json_as_str(ver_node);
    if (vs) safe_strcpy(out->version, sizeof(out->version), vs);

    json_node_t *up_node = json_get(r->data, "uptime_secs");
    if (up_node) {
        int ok2 = 0;
        int64_t v = json_as_int(up_node, &ok2);
        if (ok2 && v >= 0) out->uptime_secs = (uint64_t)v;
    }

    json_node_t *sp_node = json_get(r->data, "socket_path");
    const char *sps = json_as_str(sp_node);
    if (sps) safe_strcpy(out->socket_path, sizeof(out->socket_path), sps);

    json_node_t *cp_node = json_get(r->data, "config_path");
    const char *cps = json_as_str(cp_node);
    if (cps) safe_strcpy(out->config_path, sizeof(out->config_path), cps);

    json_node_t *rt_node = json_get(r->data, "requests_total");
    if (rt_node) {
        int ok2 = 0;
        int64_t v = json_as_int(rt_node, &ok2);
        if (ok2 && v >= 0) out->requests_total = (uint64_t)v;
    }

    json_node_t *if_node = json_get(r->data, "in_flight");
    if (if_node) {
        int ok2 = 0;
        int64_t v = json_as_int(if_node, &ok2);
        if (ok2 && v >= 0) out->in_flight = (uint64_t)v;
    }

    json_node_t *aw_node = json_get(r->data, "active_watchers");
    if (aw_node) {
        int ok2 = 0;
        int64_t v = json_as_int(aw_node, &ok2);
        if (ok2 && v >= 0) out->active_watchers = (uint64_t)v;
    }

    json_node_t *ce_node = json_get(r->data, "cache_entries");
    if (ce_node) {
        int ok2 = 0;
        int64_t v = json_as_int(ce_node, &ok2);
        if (ok2 && v >= 0) out->cache_entries = (uint64_t)v;
    }

    comb_result_free(r);
    return 0;
}

comb_result_t *comb_introspect(comb_client_t *client,
                                comb_introspect_subject_t subject,
                                uint64_t duration_secs) {
    if (!client || client->fd < 0) return result_error("not connected");

    const char *subj_str = introspect_subject_str(subject);
    char *subj_e = json_escape(subj_str);
    if (!subj_e) return result_error("out of memory");

    char buf[256];
    if (duration_secs > 0) {
        snprintf(buf, sizeof(buf),
                 "{\"op\":\"introspect\",\"subject\":%s,\"duration_secs\":%llu}",
                 subj_e, (unsigned long long)duration_secs);
    } else {
        snprintf(buf, sizeof(buf),
                 "{\"op\":\"introspect\",\"subject\":%s}", subj_e);
    }
    free(subj_e);

    return do_request(client, buf);
}

int comb_status_rows(comb_client_t *client, comb_cache_row_t **rows_out,
                     size_t *n_out) {
    if (!client || client->fd < 0) return -1;
    if (!rows_out || !n_out) return -1;

    comb_result_t *r = comb_status(client);
    if (!comb_result_ok(r)) {
        comb_result_free(r);
        return -1;
    }

    /* data must be a JSON array */
    if (!r->data || r->data->type != JSON_ARRAY) {
        comb_result_free(r);
        return -1;
    }

    /* Count items */
    size_t total = r->data->n_children;
    comb_cache_row_t *rows = (comb_cache_row_t *)calloc(
        total, sizeof(comb_cache_row_t));
    if (!rows) {
        comb_result_free(r);
        return -1;
    }

    size_t count = 0;
    json_node_t *item = r->data->children;
    while (item && count < total) {
        comb_cache_row_t *row = &rows[count];
        /* calloc zeroed the struct; set sentinel defaults */
        row->decay = -1;
        row->failure_suppressed_until_unix_ms = -1;

        /* provider */
        const char *provs = json_as_str(json_get(item, "provider"));
        row->provider = strdup(provs ? provs : "");

        /* field */
        const char *flds = json_as_str(json_get(item, "field"));
        if (flds) row->field = strdup(flds);

        /* path */
        const char *paths = json_as_str(json_get(item, "path"));
        if (paths) row->path = strdup(paths);

        /* value_json — serialise the value node back to JSON text */
        json_node_t *val = json_get(item, "value");
        if (val) {
            char vbuf[1024];
            vbuf[0] = '\0';
            switch (val->type) {
                case JSON_STRING:
                    snprintf(vbuf, sizeof(vbuf), "\"%s\"", val->val.string);
                    break;
                case JSON_INT:
                    snprintf(vbuf, sizeof(vbuf), "%lld",
                             (long long)val->val.integer);
                    break;
                case JSON_FLOAT:
                    snprintf(vbuf, sizeof(vbuf), "%g", val->val.floating);
                    break;
                case JSON_BOOL:
                    snprintf(vbuf, sizeof(vbuf), "%s",
                             val->val.boolean ? "true" : "false");
                    break;
                case JSON_NULL:
                    snprintf(vbuf, sizeof(vbuf), "null");
                    break;
                default:
                    /* object/array: leave empty — no serialiser available */
                    break;
            }
            if (vbuf[0]) row->value_json = strdup(vbuf);
        }

        /* age_ms */
        json_node_t *age_node = json_get(item, "age_ms");
        if (age_node) {
            int ok2 = 0;
            int64_t v = json_as_int(age_node, &ok2);
            if (ok2 && v >= 0) row->age_ms = (uint64_t)v;
        }

        /* stale */
        json_node_t *stale_node = json_get(item, "stale");
        if (stale_node) {
            int ok2 = 0;
            row->stale = (bool)json_as_bool(stale_node, &ok2);
        }

        /* kind discriminator — wire shape: {"kind": "lifecycle", ...} object */
        json_node_t *kind_obj = json_get(item, "kind");
        if (kind_obj && kind_obj->type == JSON_OBJECT) {
            const char *kind_name = json_as_str(json_get(kind_obj, "kind"));
            row->kind = strdup(kind_name ? kind_name : "");
            if (strcmp(row->kind, "lifecycle") == 0) {
                row->has_lifecycle = true;
                {
                    int ok2 = 0;
                    int64_t v = json_as_int(json_get(kind_obj, "decay"), &ok2);
                    row->decay = ok2 ? (int)v : -1;
                }
                {
                    int ok2 = 0;
                    row->watches_files = (bool)json_as_bool(
                        json_get(kind_obj, "watches_files"), &ok2);
                }
                {
                    int ok2 = 0;
                    int64_t v = json_as_int(
                        json_get(item, "poll_interval_secs"), &ok2);
                    if (ok2 && v >= 0) row->poll_interval_secs = (uint64_t)v;
                }
                {
                    int ok2 = 0;
                    int64_t v = json_as_int(
                        json_get(item, "keep_alive_polls"), &ok2);
                    if (ok2 && v >= 0) row->keep_alive_polls = (uint32_t)v;
                }
                {
                    int ok2 = 0;
                    row->fsevents_reinstate = (bool)json_as_bool(
                        json_get(item, "fsevents_reinstate"), &ok2);
                }
            }
        } else {
            row->kind = NULL;
        }

        /* failure object */
        json_node_t *fail = json_get(item, "failure");
        if (fail && fail->type == JSON_OBJECT) {
            int ok2 = 0;
            int64_t cf = json_as_int(
                json_get(fail, "consecutive_failures"), &ok2);
            if (ok2 && cf > 0) {
                row->in_failure = true;
                row->failure_consecutive_failures = (uint32_t)cf;
            }
            json_node_t *sup = json_get(fail, "suppressed_until_unix_ms");
            if (sup) {
                int ok3 = 0;
                int64_t sv = json_as_int(sup, &ok3);
                row->failure_suppressed_until_unix_ms = ok3 ? sv : -1;
            }
        }

        count++;
        item = item->next;
    }

    comb_result_free(r);
    *rows_out = rows;
    *n_out = count;
    return 0;
}

void comb_free_cache_rows(comb_cache_row_t *rows, size_t n) {
    if (!rows) return;
    for (size_t i = 0; i < n; i++) {
        free(rows[i].provider);
        free(rows[i].path);
        free(rows[i].field);
        free(rows[i].value_json);
        free(rows[i].kind);
    }
    free(rows);
}

/* -------------------------------------------------------------------------
 * Watch — blocking poll (Option A)
 * ---------------------------------------------------------------------- */

comb_watch_handle_t *comb_watch(comb_client_t *client, const char *key,
                                 const char *path) {
    if (!client || !key) return NULL;

    /* Open a fresh dedicated connection for the watch stream */
    if (!client->socket_path) return NULL;
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return NULL;

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    if (strlen(client->socket_path) >= sizeof(addr.sun_path)) {
        close(fd);
        return NULL;
    }
    strncpy(addr.sun_path, client->socket_path, sizeof(addr.sun_path) - 1);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(fd);
        return NULL;
    }

    /* Build and send the watch request */
    char *key_e  = json_escape(key);
    char *path_e = path ? json_escape(path) : NULL;
    if (!key_e || (path && !path_e)) {
        free(key_e); free(path_e);
        close(fd);
        return NULL;
    }

    char req_buf[4096];
    if (path_e) {
        snprintf(req_buf, sizeof(req_buf),
                 "{\"op\":\"watch\",\"key\":%s,\"path\":%s}",
                 key_e, path_e);
    } else {
        snprintf(req_buf, sizeof(req_buf),
                 "{\"op\":\"watch\",\"key\":%s}", key_e);
    }
    free(key_e); free(path_e);

    if (send_line(fd, req_buf) != 0) {
        close(fd);
        return NULL;
    }

    comb_watch_handle_t *h = (comb_watch_handle_t *)malloc(
        sizeof(comb_watch_handle_t));
    if (!h) {
        close(fd);
        return NULL;
    }
    h->fd = fd;
    return h;
}

int comb_watch_next(comb_watch_handle_t *handle, comb_watch_event_t *event,
                    int timeout_ms) {
    if (!handle || handle->fd < 0) return -1;
    if (!event) return -1;

    /* Poll for readability */
    struct pollfd pfd;
    pfd.fd      = handle->fd;
    pfd.events  = POLLIN;
    pfd.revents = 0;

    int pret = poll(&pfd, 1, timeout_ms);
    if (pret < 0) {
        if (errno == EINTR) return 0; /* treat interrupted poll as timeout */
        return -1;
    }
    if (pret == 0) return 0; /* timeout */

    if (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) return -1;

    /* Data available — read one line */
    char *raw = recv_line(handle->fd);
    if (!raw) return -1;

    memset(event, 0, sizeof(*event));

    /* Parse the response */
    json_node_t *root = json_parse(raw);
    if (!root) {
        free(raw);
        return -1;
    }

    /* Check ok */
    json_node_t *ok_node = json_get(root, "ok");
    int ok = ok_node ? json_as_bool(ok_node, NULL) : 0;
    if (!ok) {
        json_free(root);
        free(raw);
        return -1;
    }

    /* data_json */
    json_node_t *data_node = json_get(root, "data");
    if (data_node && data_node->type != JSON_NULL) {
        /* Serialise scalar data nodes back to text */
        char vbuf[1024];
        vbuf[0] = '\0';
        switch (data_node->type) {
            case JSON_STRING:
                snprintf(vbuf, sizeof(vbuf), "\"%s\"", data_node->val.string);
                break;
            case JSON_INT:
                snprintf(vbuf, sizeof(vbuf), "%lld",
                         (long long)data_node->val.integer);
                break;
            case JSON_FLOAT:
                snprintf(vbuf, sizeof(vbuf), "%g", data_node->val.floating);
                break;
            case JSON_BOOL:
                snprintf(vbuf, sizeof(vbuf), "%s",
                         data_node->val.boolean ? "true" : "false");
                break;
            default:
                /* object/array: leave as empty */
                break;
        }
        safe_strcpy(event->data_json, sizeof(event->data_json), vbuf);
    }

    /* age_ms */
    json_node_t *age_node = json_get(root, "age_ms");
    if (age_node) {
        int ok2 = 0;
        int64_t v = json_as_int(age_node, &ok2);
        if (ok2 && v >= 0) event->age_ms = (uint64_t)v;
    }

    /* stale */
    json_node_t *stale_node = json_get(root, "stale");
    if (stale_node) {
        int ok2 = 0;
        event->stale = json_as_bool(stale_node, &ok2);
    }

    json_free(root);
    free(raw);
    return 1;
}

void comb_watch_free(comb_watch_handle_t *handle) {
    if (!handle) return;
    if (handle->fd >= 0) close(handle->fd);
    free(handle);
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
