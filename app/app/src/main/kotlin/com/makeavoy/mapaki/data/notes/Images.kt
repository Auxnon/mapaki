package com.makeavoy.mapaki.data.notes

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream

/** Keeps embedded images small enough that a note stays a reasonable text file. */
object Images {
    /** Long edge for images attached to notes. */
    private const val NOTE_MAX_DIM = 1600
    private const val NOTE_KEEP_AS_IS_BYTES = 600 * 1024

    /** Width of a link's cached page preview. */
    const val PREVIEW_WIDTH = 320

    /**
     * An image ready to embed in a note: small JPEGs/PNGs/GIFs are kept as
     * they are, anything bigger is re-encoded as a JPEG at most 1600px.
     */
    fun forNote(bytes: ByteArray, mime: String): Pair<ByteArray, String>? {
        val bounds = bounds(bytes) ?: return null
        val small = bytes.size <= NOTE_KEEP_AS_IS_BYTES && maxOf(bounds.first, bounds.second) <= NOTE_MAX_DIM
        if (small && orientation(bytes) == ExifInterface.ORIENTATION_NORMAL) return bytes to mime
        val jpeg = shrink(bytes, NOTE_MAX_DIM, quality = 82) ?: return null
        return jpeg to "image/jpeg"
    }

    /** A low-res JPEG preview, [PREVIEW_WIDTH] wide. */
    fun preview(bytes: ByteArray): ByteArray? = shrink(bytes, PREVIEW_WIDTH, quality = 60, widthOnly = true)

    /** Decodes no bigger than needed for a [maxDim]-pixel view; for tiles and lists. */
    fun decodeSampled(bytes: ByteArray, maxDim: Int): Bitmap? {
        val (w, h) = bounds(bytes) ?: return null
        val opts = BitmapFactory.Options().apply { inSampleSize = sampleSize(maxOf(w, h), maxDim) }
        return runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts) }.getOrNull()
    }

    private fun shrink(bytes: ByteArray, maxDim: Int, quality: Int, widthOnly: Boolean = false): ByteArray? {
        val (w, h) = bounds(bytes) ?: return null
        val limiting = if (widthOnly) w else maxOf(w, h)
        val opts = BitmapFactory.Options().apply { inSampleSize = sampleSize(limiting, maxDim) }
        var bmp = runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts) }.getOrNull() ?: return null
        val scale = maxDim.toFloat() / (if (widthOnly) bmp.width else maxOf(bmp.width, bmp.height))
        val matrix = Matrix()
        if (scale < 1f) matrix.postScale(scale, scale)
        when (orientation(bytes)) {
            ExifInterface.ORIENTATION_ROTATE_90 -> matrix.postRotate(90f)
            ExifInterface.ORIENTATION_ROTATE_180 -> matrix.postRotate(180f)
            ExifInterface.ORIENTATION_ROTATE_270 -> matrix.postRotate(270f)
        }
        if (!matrix.isIdentity) {
            bmp = Bitmap.createBitmap(bmp, 0, 0, bmp.width, bmp.height, matrix, true)
        }
        // JPEG has no alpha: flatten onto white so transparent PNGs don't turn black.
        if (bmp.hasAlpha()) {
            val flat = Bitmap.createBitmap(bmp.width, bmp.height, Bitmap.Config.ARGB_8888)
            android.graphics.Canvas(flat).apply { drawColor(android.graphics.Color.WHITE); drawBitmap(bmp, 0f, 0f, null) }
            bmp = flat
        }
        return ByteArrayOutputStream().use { out ->
            bmp.compress(Bitmap.CompressFormat.JPEG, quality, out)
            out.toByteArray()
        }
    }

    private fun bounds(bytes: ByteArray): Pair<Int, Int>? {
        val opts = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
        return if (opts.outWidth > 0 && opts.outHeight > 0) opts.outWidth to opts.outHeight else null
    }

    private fun sampleSize(size: Int, target: Int): Int {
        var s = 1
        while (size / (s * 2) >= target) s *= 2
        return s
    }

    private fun orientation(bytes: ByteArray): Int = runCatching {
        ExifInterface(ByteArrayInputStream(bytes))
            .getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)
    }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
}
