package com.makeavoy.mapaki.data.net

import android.content.Context
import android.net.wifi.WifiManager

/** The dog emoji SSID that marks the home network. */
private const val HOME_SSID = "🐕"

const val LAN_BASE_URL = "http://beex.local:8420/"
const val REMOTE_BASE_URL = "https://mapaki.makeavoy.com/"

/** LAN when on the home SSID, the public fallback otherwise. No auto-fallback between the two. */
fun currentBaseUrl(context: Context): String =
    if (currentSsid(context) == HOME_SSID) LAN_BASE_URL else REMOTE_BASE_URL

fun isOnHomeNetwork(context: Context): Boolean = currentSsid(context) == HOME_SSID

private fun currentSsid(context: Context): String? {
    val wifiManager =
        context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
            ?: return null
    val info = try {
        wifiManager.connectionInfo
    } catch (_: SecurityException) {
        null
    } ?: return null

    var ssid = info.ssid ?: return null
    if (ssid == "<unknown ssid>" || ssid.isEmpty()) return null
    if (ssid.length >= 2 && ssid.startsWith("\"") && ssid.endsWith("\"")) {
        ssid = ssid.substring(1, ssid.length - 1)
    }
    return ssid
}
