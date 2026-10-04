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

/* Register a host log sink. Every failure recorded in kine_last_error and
 * every font registration is reported through it — levels: 0 info, 1 warn,
 * 2 error. May fire on ANY thread; the message pointer is valid only for the
 * duration of the call. NULL unregisters. */
void kine_set_log_callback(void (*callback)(int32_t level, const char *message));

/* Register a font (TTF/OTF) into the bundled-only collection.
 * Returns 0 on success, -1 on error (see kine_last_error). Idempotent. */
int32_t kine_register_font(const uint8_t *bytes, size_t len);

/* Declare the render-fallback family: with one set, a document naming an
 * unregistered family still renders (text shaped by the fallback, WARNED once
 * per family through the log sink); without one a missing family is a hard
 * render error. Probe `missingFonts` stays strict either way. The name must
 * already resolve (the embedded "Inter" always does); "" clears back to
 * strict. Returns 0, or -1 (see kine_last_error). */
int32_t kine_set_fallback_family(const char *name);


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
 * { "version", "size", "inputs": [...], "roles": [...], "assets": [...],
 *   "fonts": [...], "missingFonts": [...] }. */
kine_buf kine_probe(const char *doc_json);

/* Admit an AUTHOR's document (SCHEMA §10): repair what has one reading, then
 * validate. Returns JSON { "document": "<json>", "repairs": [{ "path", "rule",
 * "message" }], "interface": { ...as kine_probe... } }; the document text is
 * the author's own bytes when nothing was repaired. */
kine_buf kine_admit(const char *doc_json);

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

/* The grown canvas the document's TEXT content needs with these signals —
 * JSON {"width","height","y"} in design units (width = doc width, height >=
 * doc height, y <= 0 = grown top in doc coords). Doc-sized at y 0 =
 * everything fits. The one definition of text overflow; pair with
 * kine_document_render_rgba_viewport to render without cropping. */
kine_buf kine_document_layout_size(int64_t handle, const char *signals_json);

/* kine_document_render_rgba over a vertical design-space viewport
 * (view_y, view_h) — pass layout_size's y/height with a matching-aspect
 * target; (0, doc height) is exactly the plain render. */
kine_buf kine_document_render_rgba_viewport(int64_t handle, double t,
                                            const char *signals_json,
                                            uint32_t width, uint32_t height,
                                            double view_y, double view_h);

/* Union INK rect across `samples` frames over [0, span] seconds at a probe
 * raster — JSON {"x","y","width","height","canvasWidth","canvasHeight",
 * "canvasY"}: x/y/width/height are fractions of the GROWN canvas
 * (canvasWidth × canvasHeight px), canvasY is the grown canvas's offset from
 * the design box in design-height fractions; empty buffer = fully blank.
 * The one definition of visual bounds for all hosts. */
kine_buf kine_document_ink_union(int64_t handle, const char *signals_json,
                                 uint32_t samples, double span, uint32_t width,
                                 uint32_t height);

/* One-shot ink union for hosts without the handle lifecycle (Ruby gem's
 * registration-time probes) — compiles per call. */
kine_buf kine_ink_union(const char *doc_json, const char *signals_json,
                        uint32_t samples, double span, uint32_t width,
                        uint32_t height);

/* Free a document handle. Idempotent; a use-after-free reports an error. */
void kine_document_free(int64_t handle);

/* ---- GPU flavor (Apple, built with --features gpu) ----------------------- */

/* Whether this build carries the GPU flavor. ALWAYS present — hosts branch on
 * this rather than probing for symbols, because a CPU-only build (the ruby gem,
 * the rails server) has none of the kine_gpu_* entry points below. Returns 1
 * when the flavor is compiled in, 0 otherwise. */
int32_t kine_gpu_available(void);

/* The entry points below exist only when kine_gpu_available() returns 1.
 *
 * The GPU flavor renders on the HOST's MTLDevice and MTLCommandQueue, into a
 * texture the HOST creates and owns. No Metal object changes ownership across
 * this boundary in either direction: kine retains the device and queue for the
 * engine's lifetime and borrows the target texture for the duration of a render.
 *
 * Sharing the host's queue is what orders kine's raster against the host's own
 * command buffers — which is why the queue is a creation parameter and why the
 * render call submits and RETURNS rather than waiting for the GPU.
 *
 * Output is byte-shaped exactly like kine_document_render_rgba: premultiplied
 * RGBA8, sRGB. It is NOT bit-identical to the CPU flavor (different raster
 * back-ends over the same geometry); the crate's parity gate is PSNR >= 50 dB
 * with <= 0.05% of pixels differing by more than 8/255. */

/* Create the GPU engine on the host's device and queue (both required; both are
 * id<MTLDevice> / id<MTLCommandQueue>). Returns the handle (> 0), or 0 on
 * failure — see kine_last_error(). A device that is not the system default is
 * REFUSED rather than silently crossed.
 *
 * One engine per process is the intended shape: it owns the image atlas and the
 * renderer. Free with kine_gpu_engine_destroy. */
int64_t kine_gpu_engine_create(void *mtl_device, void *mtl_queue);

/* Destroy a GPU engine. Idempotent; a use-after-free reports an error. */
void kine_gpu_engine_destroy(int64_t engine);

/* Rasterize a document handle at time `t` into a host-owned id<MTLTexture>.
 * Returns 0 on success, -1 on failure (see kine_last_error()).
 *
 * The texture must be MTLPixelFormatRGBA8Unorm, exactly width x height, and
 * carry MTLTextureUsageRenderTarget; all three are checked. On ANY failure the
 * texture is left untouched — the host falls back to the CPU flavor for that
 * frame. Failure is a normal outcome, not an exception: an exhausted image
 * atlas reports here rather than aborting.
 *
 * Renders serialize on the engine (it owns mutable atlas + renderer state). */
int32_t kine_gpu_render_document(int64_t engine, int64_t document, double t,
                                 const char *signals_json, uint32_t width,
                                 uint32_t height, void *mtl_texture);

/* kine_gpu_render_document over a vertical design-space viewport — the GPU twin
 * of kine_document_render_rgba_viewport. */
int32_t kine_gpu_render_document_viewport(int64_t engine, int64_t document,
                                          double t, const char *signals_json,
                                          uint32_t width, uint32_t height,
                                          double view_y, double view_h,
                                          void *mtl_texture);

/* ---- GPU flavor (Android, built with --features gpu-gles) ----------------- */

/* The same flavor over wgpu's GLES backend. The engine is built on the EGL
 * context CURRENT on the calling thread and renders into a GL texture the HOST
 * creates and owns. kine's commands run in that context in issue order with the
 * host's own, so a render submits and returns; the host's next draw samples the
 * finished pixels. Every call on a GLES engine — renders and the final
 * kine_gpu_engine_destroy — must come from a thread where that context is
 * current, and an engine is destroyed before its context.
 *
 * Creating the engine and every render — success or failure — leave the
 * context at GLES defaults for everything kine binds or enables: program,
 * framebuffer, vertex array and buffer bindings (indexed uniform slots
 * included) unbound; textures and sampler objects unbound on units 0-31, unit 0
 * active; scissor test and blending off, blending back to GL_FUNC_ADD, GL_ONE,
 * GL_ZERO; pixel store alignment 4, row lengths and image height 0. The
 * viewport and scissor box stay at the last target's size. A host that keeps
 * other state current re-establishes it.
 *
 * kine reads the GL errors it raises, so none reaches the host's next check.
 * Creating the engine clears GL_INVALID_ENUM from wgpu's capability probe; any
 * other error fails the creation. A render that raised any error fails (-1):
 * a pass that could not draw leaves garbage in the target. Errors already
 * pending at either point are the host's — logged through the sink and
 * cleared.
 *
 * Output and parity are those of the Apple flavor above. */

/* Create the GPU engine on the calling thread's current EGL context. Returns the
 * handle (> 0), or 0 on failure — see kine_last_error(). Free with
 * kine_gpu_engine_destroy, with the same context current. */
int64_t kine_gpu_gles_engine_create(void);

/* Rasterize a document handle at time `t` into a host-owned GL texture. Returns
 * 0 on success, -1 on failure (see kine_last_error()). `gl_texture` must name a
 * complete GL_TEXTURE_2D of GL_RGBA8, exactly width x height, one level, of the
 * engine's context or its share group — the caller's contract, since GL cannot
 * check it without a stall. On ANY failure the texture is left untouched. */
int32_t kine_gpu_gles_render_document(int64_t engine, int64_t document, double t,
                                      const char *signals_json, uint32_t width,
                                      uint32_t height, uint32_t gl_texture);

/* kine_gpu_gles_render_document over a vertical design-space viewport. */
int32_t kine_gpu_gles_render_document_viewport(int64_t engine, int64_t document,
                                               double t, const char *signals_json,
                                               uint32_t width, uint32_t height,
                                               double view_y, double view_h,
                                               uint32_t gl_texture);

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
