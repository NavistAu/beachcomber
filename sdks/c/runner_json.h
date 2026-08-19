/*
 * runner_json.h — minimal JSON parser for conformance_runner.c ONLY.
 *
 * This is not part of the beachcomber C SDK. The SDK itself (beachcomber.h)
 * is a direct binding over libbeachcomber's C ABI: every bc_* call returns a
 * caller-owned JSON envelope string, and callers use their own JSON library
 * to read it — the SDK ships no JSON code (sdks/c/json.c and json.h, which
 * did, were deleted; see Phase 5 of
 * docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md).
 *
 * The conformance runner is different: it *is* a JSON consumer (it has to
 * both build request payloads from fixture JSON and inspect the bc_*
 * envelope strings it gets back), so it needs some parser. Reusing a
 * general-purpose one from outside the tree would add an external
 * dependency to a runner that CI builds with a bare `cc`; this file is the
 * simplest correct alternative. It is deliberately built only into
 * conformance_runner (see Makefile) and installs nowhere.
 *
 * Handles JSON objects, arrays, strings, numbers (integer and float),
 * booleans, and null.
 *
 * Memory model: json_parse() allocates a tree of json_node_t. The entire
 * tree is freed with json_free(root). Strings are NUL-terminated copies
 * allocated alongside the node. Callers must not free individual nodes.
 */

#ifndef BEACHCOMBER_RUNNER_JSON_H
#define BEACHCOMBER_RUNNER_JSON_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Node types */
typedef enum {
    JSON_NULL    = 0,
    JSON_BOOL    = 1,
    JSON_INT     = 2,   /* integer number */
    JSON_FLOAT   = 3,   /* floating-point number */
    JSON_STRING  = 4,
    JSON_ARRAY   = 5,
    JSON_OBJECT  = 6,
} json_type_t;

/*
 * A single JSON value node.
 *
 * For objects and arrays, `children` is a linked list via `next`.
 * Object members carry a `key` string; array elements have key == NULL.
 */
typedef struct json_node {
    json_type_t type;

    /* Key for object members (NULL for array elements and root). */
    char *key;

    /* Value payload — valid field depends on `type`. */
    union {
        int         boolean;   /* JSON_BOOL: 0 or 1 */
        int64_t     integer;   /* JSON_INT */
        double      floating;  /* JSON_FLOAT */
        char       *string;    /* JSON_STRING: NUL-terminated */
    } val;

    /* Byte length of val.string, excluding the terminator -- set only
     * for JSON_STRING. A unicode escape for codepoint U+0000 decodes to
     * a literal NUL byte in the middle of val.string, which a
     * NUL-terminated C string can still hold, but strlen()/strcmp() go
     * blind past it. Comparisons that need to be correct for such a
     * value (see nodes_equal()) must use this length with memcmp()
     * instead. */
    size_t str_len;

    /* First child (for JSON_OBJECT / JSON_ARRAY). */
    struct json_node *children;
    /* Count of direct children. */
    size_t n_children;

    /* Next sibling in a parent's child list. */
    struct json_node *next;
} json_node_t;

/*
 * Parse a NUL-terminated JSON string.
 * Returns a heap-allocated node tree, or NULL on parse error.
 * Free with json_free().
 */
json_node_t *json_parse(const char *src);

/* Free a node tree returned by json_parse(). Safe to call with NULL. */
void json_free(json_node_t *node);

/*
 * Lookup an object member by key.
 * Returns NULL if `node` is not a JSON_OBJECT or the key is absent.
 */
json_node_t *json_get(const json_node_t *node, const char *key);

/*
 * Convenience accessors. All return 0/NULL on type mismatch.
 *
 * json_as_str   — returns the string value or NULL
 * json_as_int   — returns the integer value; *ok (if non-NULL) = 1 on success
 * json_as_float — returns the float value; *ok (if non-NULL) = 1 on success
 * json_as_bool  — returns 0 or 1; *ok (if non-NULL) = 1 on success
 */
const char *json_as_str  (const json_node_t *node);
int64_t     json_as_int  (const json_node_t *node, int *ok);
double      json_as_float(const json_node_t *node, int *ok);
int         json_as_bool (const json_node_t *node, int *ok);

/* Byte length of a JSON_STRING node's value, excluding the terminator. 0 for
 * any other type (including an empty string, so callers comparing values
 * must not use this alone to detect type mismatch). See json_node_t.str_len
 * for why this exists. */
size_t json_as_str_len(const json_node_t *node);

/* Return the JSON type name as a C string (for error messages). */
const char *json_type_name(json_type_t type);

#ifdef __cplusplus
}
#endif

#endif /* BEACHCOMBER_RUNNER_JSON_H */
