/* kine — shared render core for the Ferment motion subsystem.
 *
 * Single source of truth for the extern-C boundary that every host binding
 * links against (FFI language bindings against the cdylib, native apps against
 * the staticlib). Keep this in lockstep with src/capi.rs.
 *
 * Conventions:
 *   - A kine_buf of {NULL, 0} signals failure; call kine_last_error().
 *   - Any non-null kine_buf must be released exactly once with kine_buf_free.
 *   - JSON arguments are NUL-terminated UTF-8; invalid input is reported as an
 *     error, never a crash. The boundary is panic-proof.
 */
#ifndef KINE_H
#define KINE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Owned byte buffer. {NULL, 0} == error. */
typedef struct {
  uint8_t *ptr;
  size_t len;
} kine_buf;

/* Register a font (TTF/OTF) into the bundled-only collection.
 * Returns 0 on success, -1 on error (see kine_last_error). Idempotent. */
int32_t kine_register_font(const uint8_t *bytes, size_t len);

/* Render a v1 motion document (docs/SCHEMA.md) at time `t` with `signals_json`
 * into PNG bytes. `t` is sugar for the document's `time` input; an explicit
 * `time` signal wins. Document parsing/validation is strict; signal supply is
 * lenient (missing signals use declared defaults, unknown keys are ignored). */
kine_buf kine_render_document(const char *doc_json, double t,
                                  const char *signals_json, uint32_t width,
                                  uint32_t height);

/* Parse + validate a document and describe its interface as JSON:
 * { "version", "size", "inputs": [...], "roles": [...] }. */
kine_buf kine_probe(const char *doc_json);

/* Release a buffer returned by kine_render_document / kine_probe. */
void kine_buf_free(kine_buf buf);

/* Last error message on the calling thread, or NULL. Valid until the next
 * kine_* call on this thread. */
const char *kine_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* KINE_H */
