package com.makeavoy.mapaki.data.net

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap

/** Best-effort original filename for a content:// Uri, falling back to a generated one. */
fun displayNameFor(context: Context, uri: Uri, fallback: String): String {
    context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
        ?.use { cursor ->
            val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            if (idx >= 0 && cursor.moveToFirst()) {
                val name = cursor.getString(idx)
                if (!name.isNullOrBlank()) return name
            }
        }
    return fallback
}

fun mimeTypeFor(context: Context, uri: Uri): String =
    context.contentResolver.getType(uri) ?: "application/octet-stream"

/** Used only when we truly can't tell the type any other way — e.g. a bare screenshot stream. */
fun extensionForMime(mime: String?): String =
    mime?.let { MimeTypeMap.getSingleton().getExtensionFromMimeType(it) } ?: "png"

fun readAllBytes(resolver: ContentResolver, uri: Uri): ByteArray? =
    resolver.openInputStream(uri)?.use { it.readBytes() }
