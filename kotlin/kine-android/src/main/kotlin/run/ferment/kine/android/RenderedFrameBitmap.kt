package run.ferment.kine.android

import android.graphics.Bitmap
import java.nio.ByteBuffer
import run.ferment.kine.Kine

/**
 * A [Bitmap] backed by the frame's premultiplied RGBA bytes — the seat of the
 * Swift wrapper's `RenderedFrame.cgImage()`.
 *
 * No conversion happens: `ARGB_8888` is R,G,B,A in memory and carries
 * premultiplied alpha by default, which is exactly what
 * `Kine.Document.renderRGBA` returns (`CGImageAlphaInfo.premultipliedLast` on
 * the other side).
 */
fun Kine.RenderedFrame.toBitmap(): Bitmap {
    val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
    bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(data))
    return bitmap
}
