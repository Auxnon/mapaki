package com.makeavoy.mapaki.data.notes

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.io.File

class NoteFormatTest {
    private val sample = Note(
        title = "Groceries \"weekly\"",
        created = "2026-11-22T18:00:00Z",
        body = "Milk\n\"\"\" not the end\nback\\slash\ttab\nünïcode ✓\n![receipt](r.jpg)\nends with \"",
        images = listOf(EmbeddedImage("r.jpg", "image/jpeg", byteArrayOf(0, 1, 2, -1))),
    )

    /** Same file the desktop client's note.rs tests against: both write identical bytes. */
    @Test
    fun matchesSharedFixture() {
        val fixture = File("../../fixtures/sample.note").readText()
        assertEquals(fixture, sample.toToml())
        assertEquals(fixture, Note.parse(fixture.toByteArray()).toToml())
    }

    @Test
    fun roundTrips() {
        val back = Note.parse(sample.toBytes())
        assertEquals(sample.title, back.title)
        assertEquals(sample.body + "\n", back.body)
        assertEquals(sample.images, back.images)
    }

    @Test
    fun plainTextOpensAsBody() {
        val n = Note.parse("just text\nmore\n".toByteArray())
        assertEquals("just text\nmore\n", n.body)
        assertEquals("just text", n.displayTitle)
    }

    @Test
    fun handEditedTomlParses() {
        val src = """
            # written by hand
            title = 'Trip'   # literal string
            created = 2026-11-20T09:00:00Z
            body = '''
            Oslo
            Bergen'''
            tags = ["a", "b",]

            [[images]]
            name = "x.png"
            data = '''
            AAEC
            /w==
            '''
        """.trimIndent()
        val n = Note.parse(src.toByteArray())
        assertEquals("Trip", n.title)
        assertEquals("2026-11-20T09:00:00Z", n.created)
        assertEquals("Oslo\nBergen", n.body)
        assertArrayEquals(byteArrayOf(0, 1, 2, -1), n.images.single().data)
        assertEquals("image/jpeg", n.images.single().mime)
    }

    @Test
    fun tomlEdgeCases() {
        val src = """
            a.b = 1_000
            "quoted key" = "tab\there \u00e9 \U0001F600"
            ml = QQQ
            one ""
            line \
               joinedQQQ
            q = QQQx"QQQ
            inline = { x = 1, y = { z = true } }
            f = 6.5e2
            [t]
            k = -0x10
        """.trimIndent().replace("QQQ", "\"\"\"")
        val t = Toml.parse(src)
        @Suppress("UNCHECKED_CAST")
        assertEquals(1000L, (t["a"] as Map<String, Any?>)["b"])
        assertEquals("tab\there é \uD83D\uDE00", t["quoted key"])
        assertEquals("one \"\"\nline joined", t["ml"])
        assertEquals("x\"", t["q"])
        assertEquals(650.0, t["f"])
        @Suppress("UNCHECKED_CAST")
        assertEquals(true, ((t["inline"] as Map<String, Any?>)["y"] as Map<String, Any?>)["z"])
    }

    @Test
    fun links() {
        val card = LinkCard(
            url = "https://e.com/a",
            title = "E",
            site = "e.com",
            preview = EmbeddedImage("preview.jpg", "image/jpeg", byteArrayOf(1, 2)),
        )
        val back = LinkCard.parse(card.toBytes())
        assertEquals(card, back)
        // v0.1 links were just the URL.
        val old = LinkCard.parse("https://old.example/x\n".toByteArray())
        assertEquals("https://old.example/x", old.url)
        assertNull(old.preview)
    }

    @Test
    fun imageRefs() {
        assertEquals("r.jpg", imageRef("![receipt](r.jpg)"))
        assertEquals("a b.png", imageRef("  ![](a b.png) "))
        assertNull(imageRef("see ![](x.png)"))
        assertNull(imageRef("![]()"))
    }

    @Test
    fun fileStems() {
        assertEquals("Trip- Oslo - Bergen", fileStemFor("Trip: Oslo / Bergen?", "note"))
        assert(fileStemFor("(untitled)", "note").startsWith("note-"))
    }

    @Test
    fun metaTags() {
        val html = """<html><head><title>Fallback &amp; co</title>
            <meta property="og:title" content="Real &quot;Title&quot;">
            <meta content='https://img/x.jpg' property='og:image'>
            <meta name="description" content="Desc &#8212; here"></head>"""
        val m = LinkPreview.parseMeta(html)
        assertEquals("Real \"Title\"", m["og:title"])
        assertEquals("https://img/x.jpg", m["og:image"])
        assertEquals("Desc — here", m["description"])
        assertEquals("Fallback & co", m["title"])
    }
}
