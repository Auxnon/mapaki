package com.makeavoy.mapaki.data.net

import okhttp3.Dns
import java.net.InetAddress
import java.net.UnknownHostException

/** Falls back to a manual mDNS query for `*.local` hosts the system resolver can't handle. */
class MdnsDns(private val resolver: MdnsResolver) : Dns {
    override fun lookup(hostname: String): List<InetAddress> {
        if (!hostname.endsWith(".local")) {
            return Dns.SYSTEM.lookup(hostname)
        }
        return try {
            Dns.SYSTEM.lookup(hostname)
        } catch (e: UnknownHostException) {
            val ip = resolver.resolve(hostname) ?: throw e
            listOf(InetAddress.getByName(ip))
        }
    }
}
