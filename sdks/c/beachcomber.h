/*
 * beachcomber.h — C client library for the beachcomber daemon
 *
 * Usage:
 *
 *   comb_client_t *c = comb_connect();
 *   if (!c) { fprintf(stderr, "daemon not running\n"); return 1; }
 *
 *   comb_result_t *r = comb_get(c, "git.branch", "/path/to/repo");
 *   if (comb_result_ok(r) && comb_result_is_hit(r)) {
 *       printf("branch: %s\n", comb_result_get_str(r, NULL));
 *   }
 *   comb_result_free(r);
 *   comb_disconnect(c);
 *
 * Memory model:
 *   - comb_connect / comb_connect_path return a heap-allocated client.
 *     Free with comb_disconnect().
 *   - All comb_get / comb_status calls return a heap-allocated
 *     comb_result_t. Free with comb_result_free(). NULL is never returned
 *     from these functions — on allocation failure the process aborts.
 *   - Strings returned by comb_result_get_str / comb_result_error /
 *     comb_result_raw_json are owned by the result and are valid until
 *     comb_result_free() is called. Do not free them.
 *   - path and key parameters accept NULL where noted.
 */

#ifndef BEACHCOMBER_H
#define BEACHCOMBER_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------------------
 * Opaque types
 * ---------------------------------------------------------------------- */

typedef struct comb_client      comb_client_t;
typedef struct comb_result      comb_result_t;
typedef struct comb_watch_handle comb_watch_handle_t;

/* -------------------------------------------------------------------------
 * Typed response shapes
 * ---------------------------------------------------------------------- */

typedef struct comb_hello_info {
    char protocol_version[32];
    char daemon_version[32];
} comb_hello_info_t;

typedef struct comb_daemon_health {
    int64_t  pid;
    char     version[32];
    uint64_t uptime_secs;
    char     socket_path[256];
    char     config_path[256];  /* empty string when null */
    uint64_t requests_total;
    uint64_t in_flight;
    uint64_t active_watchers;
    uint64_t cache_entries;
} comb_daemon_health_t;

typedef struct comb_cache_row {
    char    *provider;    /* owned; always non-NULL */
    char    *path;        /* owned; NULL if absent */
    char    *field;       /* owned; NULL if absent */
    char    *value_json;  /* owned JSON string; NULL if absent */
    uint64_t age_ms;
    bool     stale;
    /* Phase 2.5: lifecycle / kind fields */
    char    *kind;                         /* owned snake_case kind name; NULL if absent. e.g. "lifecycle". */
    int      decay;                        /* -1 if not lifecycle, else 0-4 */
    bool     watches_files;
    uint64_t poll_interval_secs;           /* 0 if absent */
    uint32_t keep_alive_polls;             /* 0 if absent */
    bool     fsevents_reinstate;
    bool     has_lifecycle;                /* true iff lifecycle fields above are populated */
    bool     in_failure;                   /* true iff failure object present */
    uint32_t failure_consecutive_failures;
    int64_t  failure_suppressed_until_unix_ms; /* -1 if absent */
    /* Phase 5: source name within the provider */
    char    *source;                       /* owned; NULL if absent */
} comb_cache_row_t;

typedef struct comb_watch_event {
    char     data_json[1024];  /* JSON-encoded data, or "" if absent */
    uint64_t age_ms;
    int      stale;
} comb_watch_event_t;

typedef enum {
    COMB_INTROSPECT_DAEMON,
    COMB_INTROSPECT_PROVIDERS,
    COMB_INTROSPECT_CONFIG,
    COMB_INTROSPECT_CACHE,
    COMB_INTROSPECT_LIFECYCLE,
    COMB_INTROSPECT_WATCHES,
    COMB_INTROSPECT_TIMERS,
    COMB_INTROSPECT_DEMAND,
    COMB_INTROSPECT_PROCS,
} comb_introspect_subject_t;

/* -------------------------------------------------------------------------
 * Connection
 * ---------------------------------------------------------------------- */

/*
 * Connect to the beachcomber daemon, auto-discovering the socket path.
 *
 * Discovery order:
 *   1. $XDG_RUNTIME_DIR/beachcomber/sock
 *   2. $TMPDIR/beachcomber-<uid>/sock
 *   3. /tmp/beachcomber-<uid>/sock
 *
 * Returns NULL if the socket does not exist or the connection fails.
 */
comb_client_t *comb_connect(void);

/*
 * Connect to the daemon at an explicit socket path.
 * Returns NULL on failure.
 */
comb_client_t *comb_connect_path(const char *socket_path);

/*
 * Close the connection and free the client.
 * Safe to call with NULL.
 */
void comb_disconnect(comb_client_t *client);

/* -------------------------------------------------------------------------
 * Core operations
 *
 * All functions return a comb_result_t. Check comb_result_ok() first.
 * Pass NULL for path when querying a global provider (e.g. "hostname").
 * ---------------------------------------------------------------------- */

/*
 * Read a cached value from the daemon.
 *
 *   key  — e.g. "git.branch" or "git"  (required)
 *   path — directory context (optional, pass NULL for global providers)
 *
 * On a cache miss comb_result_ok() returns 1 but comb_result_is_hit()
 * returns 0.
 */
comb_result_t *comb_get(comb_client_t *client, const char *key,
                        const char *path);

/*
 * Read a cached value with optional force recompute and/or wait-for-data.
 *
 *   key   — e.g. "git.branch" (required)
 *   path  — directory context (optional)
 *   force — non-zero to trigger immediate recomputation
 *   wait  — non-zero to block until a value is available
 */
comb_result_t *comb_get_with_flags(
    comb_client_t *client,
    const char    *key,
    const char    *path,
    int            force,
    int            wait);

/*
 * Force recomputation of a provider.
 *
 *   key  — provider name, e.g. "git"  (required)
 *   path — directory context (optional)
 *
 * Returns 0 on success, -1 on error.
 */
int comb_refresh(comb_client_t *client, const char *key, const char *path);

/*
 * Set the default path for subsequent queries on this connection.
 * Subsequent comb_get calls with path == NULL will use this context.
 *
 * Returns 0 on success, -1 on error.
 */
int comb_set_context(comb_client_t *client, const char *path);

/*
 * Return typed cache rows. Allocates an array of *n_out rows into *rows_out.
 * Caller must free with comb_free_cache_rows(). Returns 0 on success, -1 on error.
 */
int comb_status(
    comb_client_t     *client,
    comb_cache_row_t **rows_out,
    size_t            *n_out);

/*
 * Query the Hello op — fills protocol_version and daemon_version.
 * Returns 0 on success, -1 on error.
 */
int comb_hello(comb_client_t *client, comb_hello_info_t *out);

/*
 * Store a virtual provider entry. data_json must be a JSON object string.
 * ttl and path are optional (pass NULL to omit).
 * Returns 0 on success, -1 on error.
 */
int comb_put(
    comb_client_t *client,
    const char    *key,
    const char    *data_json,
    const char    *ttl,
    const char    *path);

/*
 * Clear a virtual provider's cache entry (put with no data).
 * Returns 0 on success, -1 on error.
 */
int comb_put_null(comb_client_t *client, const char *key, const char *path);

/*
 * Introspect the daemon — typed fill into *out.
 * Returns 0 on success, -1 on error.
 */
int comb_introspect_daemon(comb_client_t *client, comb_daemon_health_t *out);

/*
 * Introspect any subject, returning the raw result. Caller frees with
 * comb_result_free(). duration_secs is only used for some subjects (pass 0).
 */
comb_result_t *comb_introspect(
    comb_client_t            *client,
    comb_introspect_subject_t subject,
    uint64_t                  duration_secs);


/*
 * Free an array returned by comb_status().
 * Safe to call with rows == NULL.
 */
void comb_free_cache_rows(comb_cache_row_t *rows, size_t n);

/* -------------------------------------------------------------------------
 * Watch (blocking poll — Option A)
 * ---------------------------------------------------------------------- */

/*
 * Open a watch stream for a key. Returns NULL on error.
 * The handle owns a dedicated socket connection that stays open until
 * comb_watch_free() is called.
 */
comb_watch_handle_t *comb_watch(
    comb_client_t *client,
    const char    *key,
    const char    *path);

/*
 * Read the next watch event.
 *
 * Blocks up to timeout_ms for the next event (negative = block forever).
 *
 * Returns:
 *   1  — event ready; *event filled
 *   0  — timeout (no event within timeout_ms)
 *  -1  — error or connection closed
 */
int comb_watch_next(
    comb_watch_handle_t *handle,
    comb_watch_event_t  *event,
    int                  timeout_ms);

/*
 * Close the watch stream and free the handle. Safe to call with NULL.
 */
void comb_watch_free(comb_watch_handle_t *handle);

/* -------------------------------------------------------------------------
 * Result accessors
 * ---------------------------------------------------------------------- */

/*
 * Returns 1 if the server responded with "ok": true, 0 otherwise.
 * On network / parse errors this returns 0 and comb_result_error()
 * returns a description.
 */
int comb_result_ok(const comb_result_t *r);

/*
 * Returns 1 if the response is a cache hit (data present), 0 for a miss.
 * Only meaningful when comb_result_ok() == 1.
 */
int comb_result_is_hit(const comb_result_t *r);

/*
 * Returns the error message string, or NULL if there was no error.
 * Valid until comb_result_free().
 */
const char *comb_result_error(const comb_result_t *r);

/*
 * Get a string field from the result data.
 *
 * For single-field queries (e.g. "git.branch"), pass field == NULL or the
 * field name — both work when the data value is a scalar string.
 *
 * For full-object queries (e.g. "git"), pass the field name to select a
 * member of the data object.
 *
 * Returns NULL if the field is absent or not a string.
 * The returned pointer is valid until comb_result_free().
 */
const char *comb_result_get_str(const comb_result_t *r, const char *field);

/*
 * Get an integer field. Writes to *out and returns 1 on success.
 * Returns 0 if absent, wrong type, or out is NULL.
 */
int comb_result_get_int(const comb_result_t *r, const char *field,
                        int64_t *out);

/*
 * Get a floating-point field. Writes to *out and returns 1 on success.
 * Returns 0 if absent, wrong type, or out is NULL.
 */
int comb_result_get_float(const comb_result_t *r, const char *field,
                          double *out);

/*
 * Get a boolean field. Writes 0 or 1 to *out and returns 1 on success.
 * Returns 0 if absent, wrong type, or out is NULL.
 */
int comb_result_get_bool(const comb_result_t *r, const char *field,
                         int *out);

/*
 * Return the age of the cached value in milliseconds.
 * Returns 0 on a miss or when the field is absent.
 */
uint64_t comb_result_age_ms(const comb_result_t *r);

/*
 * Returns 1 if the cached value is stale (recomputation is pending).
 * Returns 0 on a miss or when the field is absent.
 */
int comb_result_stale(const comb_result_t *r);

/*
 * Return the raw JSON response string.
 * Valid until comb_result_free(). Never NULL for a valid result.
 */
const char *comb_result_raw_json(const comb_result_t *r);

/*
 * Free a result returned by comb_get / comb_status.
 * Safe to call with NULL.
 */
void comb_result_free(comb_result_t *r);

/* -------------------------------------------------------------------------
 * Utility
 * ---------------------------------------------------------------------- */

/*
 * Resolve and return the beachcomber socket path using the same discovery
 * logic as comb_connect(). Writes at most dst_len-1 bytes (NUL-terminated).
 * Returns dst on success, NULL if dst is too small.
 */
char *comb_socket_path(char *dst, size_t dst_len);

#ifdef __cplusplus
}
#endif

#endif /* BEACHCOMBER_H */
