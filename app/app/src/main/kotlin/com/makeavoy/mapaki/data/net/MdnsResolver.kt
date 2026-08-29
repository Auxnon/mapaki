package com.makeavoy.mapaki.data.net

import android.net.wifi.WifiManager
import java.io.ByteArrayOutputStream
import java.net.DatagramPacket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.MulticastSocket
import java.net.SocketTimeoutException

/**
 * One-shot mDNS (RFC 6762) hostname resolver for `*.local` names. Android's
 * standard resolver doesn't do multicast DNS, and the daemon on beex.local is
 * only reachable that way on this network, so we speak the wire protocol
 * directly rather than pull in a whole Bonjour/JmDNS library for one lookup.
 */
class MdnsResolver(private val wifiManager: WifiManager) {

    private data class CacheEntry(val ip: String, val expiresAtMs: Long)

    private val cache = mutableMapOf<String, CacheEntry>()

    @Synchronized
    fun resolve(host: String, timeoutMs: Long = 2000): String? {
        cache[host]?.let { entry ->
            if (System.currentTimeMillis() < entry.expiresAtMs) return entry.ip
        }

        val lock = wifiManager.createMulticastLock("mapaki-mdns")
        lock.setReferenceCounted(true)
        var socket: MulticastSocket? = null
        try {
            lock.acquire()
            socket = MulticastSocket(null)
            socket.reuseAddress = true
            socket.bind(InetSocketAddress(0))
            val group = InetAddress.getByName(MDNS_GROUP)
            socket.joinGroup(InetSocketAddress(group, MDNS_PORT), null)

            val query = buildQuery(host)
            socket.send(DatagramPacket(query, query.size, group, MDNS_PORT))

            val deadline = System.currentTimeMillis() + timeoutMs
            val recvBuf = ByteArray(4096)
            while (System.currentTimeMillis() < deadline) {
                val remaining = (deadline - System.currentTimeMillis()).coerceAtLeast(1)
                socket.soTimeout = remaining.toInt()
                val packet = DatagramPacket(recvBuf, recvBuf.size)
                try {
                    socket.receive(packet)
                } catch (_: SocketTimeoutException) {
                    break
                }
                val ip = parseAResponse(recvBuf, packet.length, host)
                if (ip != null) {
                    cache[host] = CacheEntry(ip, System.currentTimeMillis() + CACHE_TTL_MS)
                    return ip
                }
            }
        } catch (_: Exception) {
            // Fall through to null — caller decides what to do with a failed lookup.
        } finally {
            try {
                socket?.close()
            } catch (_: Exception) {
            }
            try {
                lock.release()
            } catch (_: Exception) {
            }
        }
        return null
    }

    private fun buildQuery(host: String): ByteArray {
        val out = ByteArrayOutputStream()
        fun u16(v: Int) {
            out.write((v shr 8) and 0xFF)
            out.write(v and 0xFF)
        }
        u16(0) // transaction id
        u16(0) // flags: standard query
        u16(1) // qdcount
        u16(0) // ancount
        u16(0) // nscount
        u16(0) // arcount
        for (label in host.split(".")) {
            val bytes = label.toByteArray(Charsets.US_ASCII)
            out.write(bytes.size)
            out.write(bytes)
        }
        out.write(0) // root label
        u16(1) // qtype A
        u16(1) // qclass IN
        return out.toByteArray()
    }

    /** Returns (dotted-label name, offset immediately after this name occurrence). */
    private fun readName(buf: ByteArray, offset: Int): Pair<String, Int> {
        val labels = mutableListOf<String>()
        var pos = offset
        var jumped = false
        var afterPointerOffset = -1
        var guard = 0
        while (guard++ < 128 && pos < buf.size) {
            val len = buf[pos].toInt() and 0xFF
            if (len == 0) {
                pos += 1
                break
            }
            if (len and 0xC0 == 0xC0) {
                if (pos + 1 >= buf.size) break
                val pointer = ((len and 0x3F) shl 8) or (buf[pos + 1].toInt() and 0xFF)
                if (!jumped) {
                    afterPointerOffset = pos + 2
                    jumped = true
                }
                pos = pointer
                continue
            }
            val start = pos + 1
            val end = start + len
            if (end > buf.size) break
            labels.add(String(buf, start, len, Charsets.US_ASCII))
            pos = end
        }
        val nextOffset = if (jumped) afterPointerOffset else pos
        return labels.joinToString(".") to nextOffset
    }

    private fun parseAResponse(buf: ByteArray, len: Int, host: String): String? {
        if (len < 12) return null
        fun u16(off: Int) = ((buf[off].toInt() and 0xFF) shl 8) or (buf[off + 1].toInt() and 0xFF)

        val qdcount = u16(4)
        val ancount = u16(6)
        var pos = 12

        repeat(qdcount) {
            if (pos >= len) return null
            val (_, next) = readName(buf, pos)
            pos = next + 4 // qtype + qclass
        }

        repeat(ancount) {
            if (pos >= len) return@repeat
            val (name, next) = readName(buf, pos)
            pos = next
            if (pos + 10 > len) return null
            val type = u16(pos); pos += 2
            pos += 2 // class
            pos += 4 // ttl
            val rdlen = u16(pos); pos += 2
            if (pos + rdlen > len) return null
            if (type == 1 && rdlen == 4 && name.equals(host, ignoreCase = true)) {
                return "${buf[pos].toInt() and 0xFF}.${buf[pos + 1].toInt() and 0xFF}." +
                    "${buf[pos + 2].toInt() and 0xFF}.${buf[pos + 3].toInt() and 0xFF}"
            }
            pos += rdlen
        }
        return null
    }

    companion object {
        private const val MDNS_GROUP = "224.0.0.251"
        private const val MDNS_PORT = 5353
        private const val CACHE_TTL_MS = 60_000L
    }
}
