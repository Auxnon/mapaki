package com.makeavoy.mapaki.data.net

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.location.LocationManager
import android.net.wifi.WifiManager
import androidx.core.content.ContextCompat
import androidx.core.location.LocationManagerCompat

/** The dog emoji SSID that marks the home network. */
private const val HOME_SSID = "🐕"

const val LAN_BASE_URL = "http://beex.local:8420/"
const val REMOTE_BASE_URL = "https://mapaki.makeavoy.com/"

/** LAN when on the home SSID, the public fallback otherwise. No auto-fallback between the two. */
fun currentBaseUrl(context: Context): String =
    if (currentSsid(context) == HOME_SSID) LAN_BASE_URL else REMOTE_BASE_URL

fun isOnHomeNetwork(context: Context): Boolean = currentSsid(context) == HOME_SSID

/**
 * Explains *why* the SSID read might be failing — the two ways Android hides
 * it are silent (getConnectionInfo() just returns "<unknown ssid>"), which
 * looks identical to "not on the home network" unless called out explicitly.
 */
fun ssidReadIssue(context: Context): String? {
    val hasPermission = ContextCompat.checkSelfPermission(
        context,
        Manifest.permission.ACCESS_FINE_LOCATION,
    ) == PackageManager.PERMISSION_GRANTED
    if (!hasPermission) return "location permission not granted — SSID can't be read"

    val locationManager = context.getSystemService(Context.LOCATION_SERVICE) as? LocationManager
    if (locationManager != null && !LocationManagerCompat.isLocationEnabled(locationManager)) {
        return "device Location is off — SSID can't be read"
    }
    return null
}

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
