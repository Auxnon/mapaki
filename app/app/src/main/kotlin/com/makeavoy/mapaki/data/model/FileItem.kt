package com.makeavoy.mapaki.data.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
enum class FileKind {
    @SerialName("image") IMAGE,
    @SerialName("link") LINK,
    @SerialName("file") FILE,
}

@Serializable
data class FileItem(
    val id: String,
    val name: String,
    /** Unknown kinds from a newer server decode as FILE (see coerceInputValues). */
    val kind: FileKind = FileKind.FILE,
    val size: Long,
    @SerialName("added_at") val addedAt: Long,
    val position: Long,
    /** Server revision id of the current content. */
    val head: String = "",
    /** sha256 of the current content; a changed hash means re-download. */
    val hash: String = "",
    val deleted: Boolean = false,
)

/** Notes and links are TOML files told apart by extension (see data/notes). */
val FileItem.isNote: Boolean get() = name.endsWith(".note", ignoreCase = true)

val FileItem.isLink: Boolean get() = kind == FileKind.LINK || name.endsWith(".link", ignoreCase = true)
