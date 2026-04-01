/*
 * json.c — minimal JSON parser for beachcomber C SDK
 */

#include "json.h"

#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <errno.h>
#include <math.h>   /* HUGE_VAL */

/* -------------------------------------------------------------------------
 * Internal parser state
 * ---------------------------------------------------------------------- */

typedef struct {
    const char *src;
    size_t      pos;
    size_t      len;
} parser_t;

/* Forward declarations */
static json_node_t *parse_value(parser_t *p);
static json_node_t *parse_object(parser_t *p);
static json_node_t *parse_array(parser_t *p);
static json_node_t *parse_string_node(parser_t *p);
static json_node_t *parse_number(parser_t *p);
static json_node_t *parse_literal(parser_t *p, const char *lit,
                                   json_type_t type, int bool_val);
static void skip_ws(parser_t *p);
static char peek(const parser_t *p);
static char next_char(parser_t *p);

/* -------------------------------------------------------------------------
 * Node allocation helpers
 * ---------------------------------------------------------------------- */

static json_node_t *node_alloc(json_type_t type) {
    json_node_t *n = (json_node_t *)calloc(1, sizeof(json_node_t));
    if (n) n->type = type;
    return n;
}

static char *str_dup_n(const char *s, size_t len) {
    char *out = (char *)malloc(len + 1);
    if (!out) return NULL;
    memcpy(out, s, len);
    out[len] = '\0';
    return out;
}

/* -------------------------------------------------------------------------
 * Public API
 * ---------------------------------------------------------------------- */

json_node_t *json_parse(const char *src) {
    if (!src) return NULL;
    parser_t p;
    p.src = src;
    p.pos = 0;
    p.len = strlen(src);
    skip_ws(&p);
    json_node_t *root = parse_value(&p);
    return root;
}

void json_free(json_node_t *node) {
    if (!node) return;

    /* Free sibling chain first */
    json_free(node->next);

    /* Free children */
    json_free(node->children);

    /* Free owned strings */
    free(node->key);
    if (node->type == JSON_STRING) {
        free(node->val.string);
    }

    free(node);
}

json_node_t *json_get(const json_node_t *node, const char *key) {
    if (!node || node->type != JSON_OBJECT || !key) return NULL;
    for (json_node_t *c = node->children; c; c = c->next) {
        if (c->key && strcmp(c->key, key) == 0) return c;
    }
    return NULL;
}

const char *json_as_str(const json_node_t *node) {
    if (!node || node->type != JSON_STRING) return NULL;
    return node->val.string;
}

int64_t json_as_int(const json_node_t *node, int *ok) {
    if (ok) *ok = 0;
    if (!node) return 0;
    if (node->type == JSON_INT) {
        if (ok) *ok = 1;
        return node->val.integer;
    }
    /* Accept float that is a whole number */
    if (node->type == JSON_FLOAT) {
        double d = node->val.floating;
        if (d == (double)(int64_t)d) {
            if (ok) *ok = 1;
            return (int64_t)d;
        }
    }
    return 0;
}

double json_as_float(const json_node_t *node, int *ok) {
    if (ok) *ok = 0;
    if (!node) return 0.0;
    if (node->type == JSON_FLOAT) {
        if (ok) *ok = 1;
        return node->val.floating;
    }
    if (node->type == JSON_INT) {
        if (ok) *ok = 1;
        return (double)node->val.integer;
    }
    return 0.0;
}

int json_as_bool(const json_node_t *node, int *ok) {
    if (ok) *ok = 0;
    if (!node || node->type != JSON_BOOL) return 0;
    if (ok) *ok = 1;
    return node->val.boolean;
}

const char *json_type_name(json_type_t type) {
    switch (type) {
        case JSON_NULL:   return "null";
        case JSON_BOOL:   return "bool";
        case JSON_INT:    return "int";
        case JSON_FLOAT:  return "float";
        case JSON_STRING: return "string";
        case JSON_ARRAY:  return "array";
        case JSON_OBJECT: return "object";
        default:          return "unknown";
    }
}

/* -------------------------------------------------------------------------
 * Internal parser implementation
 * ---------------------------------------------------------------------- */

static void skip_ws(parser_t *p) {
    while (p->pos < p->len && isspace((unsigned char)p->src[p->pos]))
        p->pos++;
}

static char peek(const parser_t *p) {
    if (p->pos >= p->len) return '\0';
    return p->src[p->pos];
}

static char next_char(parser_t *p) {
    if (p->pos >= p->len) return '\0';
    return p->src[p->pos++];
}

/* Parse a 4-hex-digit Unicode escape, writing UTF-8 into buf.
 * Returns number of bytes written, or 0 on error. */
static int parse_unicode_escape(parser_t *p, char *buf) {
    if (p->pos + 4 > p->len) return 0;
    unsigned int cp = 0;
    for (int i = 0; i < 4; i++) {
        char c = p->src[p->pos++];
        unsigned int digit;
        if      (c >= '0' && c <= '9') digit = (unsigned int)(c - '0');
        else if (c >= 'a' && c <= 'f') digit = (unsigned int)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F') digit = (unsigned int)(c - 'A' + 10);
        else return 0;
        cp = (cp << 4) | digit;
    }
    /* Encode as UTF-8 */
    if (cp < 0x80) {
        buf[0] = (char)cp;
        return 1;
    } else if (cp < 0x800) {
        buf[0] = (char)(0xC0 | (cp >> 6));
        buf[1] = (char)(0x80 | (cp & 0x3F));
        return 2;
    } else {
        buf[0] = (char)(0xE0 | (cp >> 12));
        buf[1] = (char)(0x80 | ((cp >> 6) & 0x3F));
        buf[2] = (char)(0x80 | (cp & 0x3F));
        return 3;
    }
}

/*
 * Parse a JSON string value (the quotes are consumed here).
 * Returns a malloc'd NUL-terminated string, or NULL on error.
 */
static char *parse_string_raw(parser_t *p) {
    if (peek(p) != '"') return NULL;
    next_char(p); /* consume opening quote */

    /* We accumulate the decoded string in a dynamically-grown buffer. */
    size_t cap = 64;
    size_t len = 0;
    char *buf = (char *)malloc(cap);
    if (!buf) return NULL;

#define APPEND(c) do {                             \
    if (len + 1 >= cap) {                         \
        cap *= 2;                                  \
        char *nb = (char *)realloc(buf, cap);      \
        if (!nb) { free(buf); return NULL; }       \
        buf = nb;                                  \
    }                                              \
    buf[len++] = (c);                              \
} while (0)

#define APPEND_N(s, n) do {                        \
    while (len + (n) + 1 >= cap) {                \
        cap *= 2;                                  \
        char *nb = (char *)realloc(buf, cap);      \
        if (!nb) { free(buf); return NULL; }       \
        buf = nb;                                  \
    }                                              \
    memcpy(buf + len, s, n);                       \
    len += (n);                                    \
} while (0)

    for (;;) {
        if (p->pos >= p->len) { free(buf); return NULL; } /* unterminated */
        char c = next_char(p);
        if (c == '"') break; /* end of string */
        if (c != '\\') {
            APPEND(c);
            continue;
        }
        /* Escape sequence */
        if (p->pos >= p->len) { free(buf); return NULL; }
        char esc = next_char(p);
        switch (esc) {
            case '"':  APPEND('"');  break;
            case '\\': APPEND('\\'); break;
            case '/':  APPEND('/');  break;
            case 'b':  APPEND('\b'); break;
            case 'f':  APPEND('\f'); break;
            case 'n':  APPEND('\n'); break;
            case 'r':  APPEND('\r'); break;
            case 't':  APPEND('\t'); break;
            case 'u': {
                char ubuf[4];
                int nb = parse_unicode_escape(p, ubuf);
                if (nb == 0) { free(buf); return NULL; }
                APPEND_N(ubuf, (size_t)nb);
                break;
            }
            default: { free(buf); return NULL; }
        }
    }

#undef APPEND
#undef APPEND_N

    buf[len] = '\0';
    return buf;
}

static json_node_t *parse_string_node(parser_t *p) {
    char *s = parse_string_raw(p);
    if (!s) return NULL;
    json_node_t *n = node_alloc(JSON_STRING);
    if (!n) { free(s); return NULL; }
    n->val.string = s;
    return n;
}

static json_node_t *parse_number(parser_t *p) {
    size_t start = p->pos;
    int is_float = 0;

    if (peek(p) == '-') p->pos++;

    if (peek(p) == '0') {
        p->pos++;
    } else {
        if (!isdigit((unsigned char)peek(p))) return NULL;
        while (p->pos < p->len && isdigit((unsigned char)p->src[p->pos]))
            p->pos++;
    }

    /* Fractional part */
    if (peek(p) == '.') {
        is_float = 1;
        p->pos++;
        if (!isdigit((unsigned char)peek(p))) return NULL;
        while (p->pos < p->len && isdigit((unsigned char)p->src[p->pos]))
            p->pos++;
    }

    /* Exponent */
    if (peek(p) == 'e' || peek(p) == 'E') {
        is_float = 1;
        p->pos++;
        if (peek(p) == '+' || peek(p) == '-') p->pos++;
        if (!isdigit((unsigned char)peek(p))) return NULL;
        while (p->pos < p->len && isdigit((unsigned char)p->src[p->pos]))
            p->pos++;
    }

    size_t num_len = p->pos - start;
    char *tmp = str_dup_n(p->src + start, num_len);
    if (!tmp) return NULL;

    json_node_t *n;
    if (is_float) {
        n = node_alloc(JSON_FLOAT);
        if (n) n->val.floating = strtod(tmp, NULL);
    } else {
        n = node_alloc(JSON_INT);
        if (n) n->val.integer = (int64_t)strtoll(tmp, NULL, 10);
    }
    free(tmp);
    return n;
}

static json_node_t *parse_literal(parser_t *p, const char *lit,
                                   json_type_t type, int bool_val) {
    size_t lit_len = strlen(lit);
    if (p->pos + lit_len > p->len) return NULL;
    if (strncmp(p->src + p->pos, lit, lit_len) != 0) return NULL;
    p->pos += lit_len;
    json_node_t *n = node_alloc(type);
    if (n && type == JSON_BOOL) n->val.boolean = bool_val;
    return n;
}

static json_node_t *parse_object(parser_t *p) {
    if (peek(p) != '{') return NULL;
    next_char(p);

    json_node_t *obj = node_alloc(JSON_OBJECT);
    if (!obj) return NULL;

    skip_ws(p);
    if (peek(p) == '}') {
        next_char(p);
        return obj;
    }

    json_node_t *tail = NULL;

    for (;;) {
        skip_ws(p);
        if (peek(p) != '"') { json_free(obj); return NULL; }

        /* Parse key */
        char *key = parse_string_raw(p);
        if (!key) { json_free(obj); return NULL; }

        skip_ws(p);
        if (next_char(p) != ':') { free(key); json_free(obj); return NULL; }
        skip_ws(p);

        /* Parse value */
        json_node_t *val = parse_value(p);
        if (!val) { free(key); json_free(obj); return NULL; }
        val->key = key;

        /* Append to children list */
        if (!obj->children) {
            obj->children = val;
        } else {
            tail->next = val;
        }
        tail = val;
        obj->n_children++;

        skip_ws(p);
        char sep = peek(p);
        if (sep == '}') { next_char(p); break; }
        if (sep == ',') { next_char(p); continue; }
        json_free(obj);
        return NULL;
    }

    return obj;
}

static json_node_t *parse_array(parser_t *p) {
    if (peek(p) != '[') return NULL;
    next_char(p);

    json_node_t *arr = node_alloc(JSON_ARRAY);
    if (!arr) return NULL;

    skip_ws(p);
    if (peek(p) == ']') {
        next_char(p);
        return arr;
    }

    json_node_t *tail = NULL;

    for (;;) {
        skip_ws(p);
        json_node_t *val = parse_value(p);
        if (!val) { json_free(arr); return NULL; }

        if (!arr->children) {
            arr->children = val;
        } else {
            tail->next = val;
        }
        tail = val;
        arr->n_children++;

        skip_ws(p);
        char sep = peek(p);
        if (sep == ']') { next_char(p); break; }
        if (sep == ',') { next_char(p); continue; }
        json_free(arr);
        return NULL;
    }

    return arr;
}

static json_node_t *parse_value(parser_t *p) {
    skip_ws(p);
    char c = peek(p);

    if (c == '{') return parse_object(p);
    if (c == '[') return parse_array(p);
    if (c == '"') return parse_string_node(p);
    if (c == 't') return parse_literal(p, "true",  JSON_BOOL, 1);
    if (c == 'f') return parse_literal(p, "false", JSON_BOOL, 0);
    if (c == 'n') return parse_literal(p, "null",  JSON_NULL, 0);
    if (c == '-' || isdigit((unsigned char)c)) return parse_number(p);

    return NULL; /* unrecognised character */
}
