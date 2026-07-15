/* kine — a render core for motion documents.
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
 *
 * Threading:
 *   - Document handles are immutable after creation. Concurrent renders on the
 *     same or different handles, from any thread, are safe; per-render state is
 *     created per call. A freed handle simply reports an error on reuse.
 *   - The font collection is process-global. kine_register_font is idempotent
 *     and safe to call concurrently with renders.
 *   - kine_last_error is thread-local; each thread sees only its own last error.
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

/* Crate + schema version, e.g. "kine 0.1.0 (schema v1)". Static; never freed. */
const char *kine_version(void);

/* ---- one-shot (parse every call; server convenience) --------------------- */

/* Render a v1 motion document (docs/SCHEMA.md) at time `t` with `signals_json`
 * into PNG bytes (RGBA8, straight/un-premultiplied alpha). `t` is sugar for the
 * document's `time` input; an explicit `time` signal wins. Document parsing and
 * validation are strict; signal supply is lenient (missing signals use declared
 * defaults, unknown keys are ignored). */
kine_buf kine_render_document(const char *doc_json, double t,
                              const char *signals_json, uint32_t width,
                              uint32_t height);

/* Like kine_render_document, but returns raw pixels for texture upload:
 * RGBA8, premultiplied alpha, sRGB, row-major, stride = width*4,
 * len = width*height*4. (Note: premultiplied, unlike the PNG output.) */
kine_buf kine_render_document_rgba(const char *doc_json, double t,
                                   const char *signals_json, uint32_t width,
                                   uint32_t height);

/* Parse + validate a document and describe its interface as JSON:
 * { "version", "size", "inputs": [...], "roles": [...] }. */
kine_buf kine_probe(const char *doc_json);

/* ---- handles (parse once; render every frame) ---------------------------- */

/* Parse + validate a document and keep it as a reusable handle. Returns the
 * handle (> 0), or 0 on error (see kine_last_error). Free with
 * kine_document_free. */
int64_t kine_document_create(const char *doc_json);

/* Describe a handle's interface (same payload as kine_probe). */
kine_buf kine_document_probe(int64_t handle);

/* Render a handle at time `t` with `signals_json` into raw RGBA8
 * (premultiplied, sRGB, row-major, stride = width*4). */
kine_buf kine_document_render_rgba(int64_t handle, double t,
                                   const char *signals_json, uint32_t width,
                                   uint32_t height);

/* Free a document handle. Idempotent; a use-after-free reports an error. */
void kine_document_free(int64_t handle);

/* ---- shared ------------------------------------------------------------- */

/* Release a buffer returned by any kine_* function above. */
void kine_buf_free(kine_buf buf);

/* Last error message on the calling thread, or NULL. Valid until the next
 * kine_* call on this thread. */
const char *kine_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* KINE_H */
