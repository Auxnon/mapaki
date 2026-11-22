package com.makeavoy.mapaki.data.notes

/**
 * Just enough TOML to read `.note` and `.link` files, including ones edited
 * by hand on a desktop: comments, bare/quoted/dotted keys, `[table]` and
 * `[[array of tables]]` headers, all four string forms, integers, floats,
 * booleans, arrays and inline tables. Dates and times come back as their
 * source text. Writing is done by [NoteFormat], which emits one fixed layout.
 */
object Toml {
    class ParseException(message: String) : Exception(message)

    fun parse(text: String): Map<String, Any?> = Parser(text).document()

    private class Parser(private val s: String) {
        private var i = 0
        private val root = LinkedHashMap<String, Any?>()

        fun document(): Map<String, Any?> {
            var table: MutableMap<String, Any?> = root
            while (true) {
                skipBlankLines()
                if (i >= s.length) return root
                if (s[i] == '[') {
                    val arrayTable = s.startsWith("[[", i)
                    i += if (arrayTable) 2 else 1
                    skipSpaces()
                    val path = key()
                    expect(if (arrayTable) "]]" else "]")
                    table = if (arrayTable) appendTable(path) else descend(root, path)
                } else {
                    val path = key()
                    skipSpaces()
                    expect("=")
                    skipSpaces()
                    val target = descend(table, path.dropLast(1))
                    target[path.last()] = value()
                }
                endOfLine()
            }
        }

        private fun appendTable(path: List<String>): MutableMap<String, Any?> {
            val parent = descend(root, path.dropLast(1))
            @Suppress("UNCHECKED_CAST")
            val list = parent.getOrPut(path.last()) { ArrayList<Any?>() } as? MutableList<Any?>
                ?: fail("${path.last()} is not an array of tables")
            return LinkedHashMap<String, Any?>().also { list.add(it) }
        }

        @Suppress("UNCHECKED_CAST")
        private fun descend(from: MutableMap<String, Any?>, path: List<String>): MutableMap<String, Any?> {
            var t = from
            for (k in path) {
                t = when (val existing = t.getOrPut(k) { LinkedHashMap<String, Any?>() }) {
                    is MutableMap<*, *> -> existing as MutableMap<String, Any?>
                    // `[a.b]` after `[[a]]` means the latest table in array `a`.
                    is MutableList<*> -> existing.lastOrNull() as? MutableMap<String, Any?> ?: fail("bad table $k")
                    else -> fail("$k is not a table")
                }
            }
            return t
        }

        private fun key(): List<String> {
            val parts = mutableListOf<String>()
            while (true) {
                skipSpaces()
                parts += when (peek()) {
                    '"' -> basicString()
                    '\'' -> literalString()
                    else -> {
                        val start = i
                        while (i < s.length && (s[i].isLetterOrDigit() || s[i] == '_' || s[i] == '-')) i++
                        if (start == i) fail("expected a key")
                        s.substring(start, i)
                    }
                }
                skipSpaces()
                if (peek() == '.') i++ else return parts
            }
        }

        private fun value(): Any? = when {
            s.startsWith("\"\"\"", i) -> multilineBasic()
            s.startsWith("'''", i) -> multilineLiteral()
            peek() == '"' -> basicString()
            peek() == '\'' -> literalString()
            peek() == '[' -> array()
            peek() == '{' -> inlineTable()
            s.startsWith("true", i) -> { i += 4; true }
            s.startsWith("false", i) -> { i += 5; false }
            else -> scalar()
        }

        private fun scalar(): Any {
            val start = i
            // Dates may contain one space between date and time: "1979-05-27 07:32:00Z".
            while (i < s.length && s[i] !in ",]}#\n\r" &&
                !(s[i] == ' ' && !(i + 1 < s.length && s[i + 1].isDigit() && s[i - 1].isDigit()))
            ) i++
            val raw = s.substring(start, i).trim()
            if (raw.isEmpty()) fail("expected a value")
            val n = raw.replace("_", "")
            return when {
                n.startsWith("0x") -> n.drop(2).toLong(16)
                n.startsWith("0o") -> n.drop(2).toLong(8)
                n.startsWith("0b") -> n.drop(2).toLong(2)
                else -> n.toLongOrNull() ?: n.toDoubleOrNull() ?: when (n) {
                    "inf", "+inf" -> Double.POSITIVE_INFINITY
                    "-inf" -> Double.NEGATIVE_INFINITY
                    "nan", "+nan", "-nan" -> Double.NaN
                    else -> raw // a date/time: keep its text
                }
            }
        }

        private fun array(): List<Any?> {
            expect("[")
            val out = ArrayList<Any?>()
            while (true) {
                skipWhitespaceAndComments()
                if (peek() == ']') { i++; return out }
                out += value()
                skipWhitespaceAndComments()
                when (peek()) {
                    ',' -> i++
                    ']' -> { i++; return out }
                    else -> fail("expected , or ] in array")
                }
            }
        }

        private fun inlineTable(): Map<String, Any?> {
            expect("{")
            val out = LinkedHashMap<String, Any?>()
            skipSpaces()
            if (peek() == '}') { i++; return out }
            while (true) {
                val path = key()
                skipSpaces()
                expect("=")
                skipSpaces()
                descend(out, path.dropLast(1))[path.last()] = value()
                skipSpaces()
                when (peek()) {
                    ',' -> i++
                    '}' -> { i++; return out }
                    else -> fail("expected , or } in inline table")
                }
            }
        }

        private fun basicString(): String {
            expect("\"")
            val sb = StringBuilder()
            while (true) {
                val c = s.getOrNull(i++) ?: fail("unterminated string")
                when (c) {
                    '"' -> return sb.toString()
                    '\\' -> escape(sb)
                    '\n' -> fail("newline in string")
                    else -> sb.append(c)
                }
            }
        }

        private fun literalString(): String {
            expect("'")
            val end = s.indexOf('\'', i)
            if (end < 0 || s.substring(i, end).contains('\n')) fail("unterminated string")
            return s.substring(i, end).also { i = end + 1 }
        }

        private fun multilineBasic(): String {
            i += 3
            skipFirstNewline()
            val sb = StringBuilder()
            while (true) {
                if (s.startsWith("\"\"\"", i)) {
                    // Up to two quotes may sit right before the closing delimiter.
                    var quotes = 3
                    while (quotes < 5 && s.getOrNull(i + quotes) == '"') quotes++
                    repeat(quotes - 3) { sb.append('"') }
                    i += quotes
                    return sb.toString()
                }
                val c = s.getOrNull(i++) ?: fail("unterminated string")
                if (c == '\\') {
                    // Line-ending backslash: drop it and all whitespace that follows.
                    var j = i
                    while (j < s.length && (s[j] == ' ' || s[j] == '\t')) j++
                    if (j < s.length && (s[j] == '\n' || s[j] == '\r')) {
                        i = j
                        while (i < s.length && s[i].isWhitespace()) i++
                    } else {
                        escape(sb)
                    }
                } else {
                    sb.append(c)
                }
            }
        }

        private fun multilineLiteral(): String {
            i += 3
            skipFirstNewline()
            val end = s.indexOf("'''", i)
            if (end < 0) fail("unterminated string")
            var close = end
            while (close + 3 < s.length && s[close + 3] == '\'' && close < end + 2) close++
            return s.substring(i, close).also { i = close + 3 }
        }

        private fun escape(sb: StringBuilder) {
            when (val e = s.getOrNull(i++) ?: fail("bad escape")) {
                'b' -> sb.append('\b')
                't' -> sb.append('\t')
                'n' -> sb.append('\n')
                'f' -> sb.append('\u000C')
                'r' -> sb.append('\r')
                'e' -> sb.append('\u001B')
                '"' -> sb.append('"')
                '\\' -> sb.append('\\')
                'u', 'U' -> {
                    val len = if (e == 'u') 4 else 8
                    val code = s.substring(i, (i + len).coerceAtMost(s.length)).toIntOrNull(16) ?: fail("bad \\$e escape")
                    sb.appendCodePoint(code)
                    i += len
                }
                'x' -> {
                    sb.append(s.substring(i, (i + 2).coerceAtMost(s.length)).toIntOrNull(16)?.toChar() ?: fail("bad \\x escape"))
                    i += 2
                }
                else -> fail("bad escape \\$e")
            }
        }

        private fun skipFirstNewline() {
            if (s.startsWith("\r\n", i)) i += 2 else if (peek() == '\n') i++
        }

        private fun peek(): Char? = s.getOrNull(i)

        private fun skipSpaces() {
            while (i < s.length && (s[i] == ' ' || s[i] == '\t')) i++
        }

        private fun skipComment() {
            if (peek() == '#') while (i < s.length && s[i] != '\n') i++
        }

        private fun skipBlankLines() {
            while (true) {
                skipSpaces()
                skipComment()
                if (i < s.length && (s[i] == '\n' || s[i] == '\r')) i++ else return
            }
        }

        private fun skipWhitespaceAndComments() {
            while (true) {
                skipSpaces()
                skipComment()
                if (i < s.length && s[i].isWhitespace()) i++ else return
            }
        }

        private fun endOfLine() {
            skipSpaces()
            skipComment()
            if (i < s.length && s[i] != '\n' && s[i] != '\r') fail("unexpected text after value")
        }

        private fun expect(token: String) {
            if (!s.startsWith(token, i)) fail("expected $token")
            i += token.length
        }

        private fun fail(message: String): Nothing {
            val line = s.substring(0, i.coerceAtMost(s.length)).count { it == '\n' } + 1
            throw ParseException("line $line: $message")
        }
    }
}
