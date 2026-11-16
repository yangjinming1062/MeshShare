package com.meshshare.android.network

import android.app.Activity
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.net.InetAddress

private const val LOG_TAG = "MeshShareNetwork"

/**
 * Keeps MeshShare's sockets on the physical LAN when a full-tunnel VPN is the
 * Android process default network. The Rust node opens raw TCP and UDP sockets,
 * so binding the process before startup is the only common policy that also
 * covers mDNS sockets created by mdns-sd.
 */
@TauriPlugin
class NetworkPlugin(activity: Activity) : Plugin(activity) {
    private val connectivity =
        activity.applicationContext.getSystemService(ConnectivityManager::class.java)
    private val wifi = activity.applicationContext.getSystemService(WifiManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private var destroyed = false
    private var boundNetworkHandle: Long? = null
    private var multicastLock: WifiManager.MulticastLock? = null

    private val lanRequest = NetworkRequest.Builder()
        .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
        .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
        .addTransportType(NetworkCapabilities.TRANSPORT_ETHERNET)
        .build()

    private val defaultNetworkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = scheduleRebind()

        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) =
            scheduleRebind()

        override fun onLost(network: Network) = scheduleRebind()
    }

    private val lanNetworkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = scheduleRebind()

        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) =
            scheduleRebind()

        override fun onLost(network: Network) = scheduleRebind()
    }

    init {
        acquireMulticastLock()
        try {
            connectivity.registerDefaultNetworkCallback(defaultNetworkCallback, main)
            connectivity.registerNetworkCallback(lanRequest, lanNetworkCallback, main)
        } catch (error: Exception) {
            Log.w(LOG_TAG, "无法监听 Android 网络变化", error)
        }
        // This runs synchronously so the Rust listeners and mDNS daemon are
        // created with the selected network already in effect.
        rebindProcessNetwork()
    }

    @Command
    fun rebind(invoke: Invoke) {
        main.post {
            try {
                invoke.resolve(rebindProcessNetwork())
            } catch (error: Exception) {
                invoke.reject("无法绑定 MeshShare 局域网网络", error)
            }
        }
    }

    override fun onDestroy(activity: androidx.appcompat.app.AppCompatActivity) {
        destroyed = true
        try {
            connectivity.unregisterNetworkCallback(defaultNetworkCallback)
        } catch (_: Exception) {
            // The callback may not have been registered on devices that reject
            // network callbacks for the current profile.
        }
        try {
            connectivity.unregisterNetworkCallback(lanNetworkCallback)
        } catch (_: Exception) {
        }
        try {
            connectivity.bindProcessToNetwork(null)
        } catch (error: Exception) {
            Log.w(LOG_TAG, "清理 MeshShare 网络绑定失败", error)
        }
        multicastLock?.let { lock ->
            if (lock.isHeld) lock.release()
        }
        multicastLock = null
        super.onDestroy(activity)
    }

    private fun scheduleRebind() {
        if (!destroyed) main.post { if (!destroyed) rebindProcessNetwork() }
    }

    private fun rebindProcessNetwork(): JSObject {
        val target = if (isFullTunnelVpnActive()) selectLanNetwork() else null
        val targetHandle = target?.networkHandle
        if (targetHandle != boundNetworkHandle) {
            val bound = connectivity.bindProcessToNetwork(target)
            if (bound) {
                boundNetworkHandle = targetHandle
                Log.i(
                    LOG_TAG,
                    if (target == null) "MeshShare 使用系统默认网络"
                    else "MeshShare 优先使用物理局域网网络 $targetHandle",
                )
            } else {
                Log.w(LOG_TAG, "无法绑定 MeshShare 网络 $targetHandle，将使用系统默认网络")
                try {
                    connectivity.bindProcessToNetwork(null)
                } catch (error: Exception) {
                    Log.w(LOG_TAG, "清理失败的 MeshShare 网络绑定失败", error)
                }
                boundNetworkHandle = null
            }
        }
        return JSObject().apply {
            put("mode", if (target == null) "default" else "lan")
            put("network_handle", targetHandle ?: 0L)
        }
    }

    private fun isFullTunnelVpnActive(): Boolean {
        val active = connectivity.activeNetwork ?: return false
        val capabilities = connectivity.getNetworkCapabilities(active) ?: return false
        if (!capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) return false

        val linkProperties = connectivity.getLinkProperties(active)
        if (linkProperties?.isTailscaleNetwork() == true) {
            Log.i(LOG_TAG, "检测到 Tailscale 组网，保留 Tailscale 网络")
            return false
        }
        // Only a VPN that installs a default route is treated as full-tunnel.
        // Split-tunnel VPNs already leave LAN routes available and should not
        // move the process away from their intended network.
        return linkProperties?.routes?.any { route ->
            route.destination.prefixLength == 0
        } ?: true
    }

    private fun LinkProperties.isTailscaleNetwork(): Boolean =
        linkAddresses.any { address -> address.address.isTailscaleAddress() } ||
            routes.any { route -> route.destination.address.isTailscaleAddress() }

    private fun InetAddress.isTailscaleAddress(): Boolean {
        val bytes = address
        if (bytes.size == 4) {
            val first = bytes[0].toInt() and 0xff
            val second = bytes[1].toInt() and 0xff
            return first == 100 && second in 64..127
        }
        return bytes.size == 16 &&
            bytes[0] == 0xfd.toByte() &&
            bytes[1] == 0x7a.toByte() &&
            bytes[2] == 0x11.toByte() &&
            bytes[3] == 0x5c.toByte() &&
            bytes[4] == 0xa1.toByte() &&
            bytes[5] == 0xe0.toByte()
    }

    private fun selectLanNetwork(): Network? {
        return connectivity.allNetworks
            .mapNotNull { network ->
                val capabilities = connectivity.getNetworkCapabilities(network) ?: return@mapNotNull null
                if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN) ||
                    !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                ) {
                    return@mapNotNull null
                }
                val rank = when {
                    capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> 0
                    capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> 1
                    else -> return@mapNotNull null
                }
                rank to network
            }
            .minWithOrNull(compareBy({ pair -> pair.first }, { pair -> pair.second.networkHandle }))
            ?.second
    }

    private fun acquireMulticastLock() {
        try {
            multicastLock = wifi?.createMulticastLock("MeshShare.mDNS")?.apply {
                setReferenceCounted(false)
                acquire()
            }
        } catch (error: Exception) {
            Log.w(LOG_TAG, "无法取得 Wi-Fi 多播锁，mDNS 可能受系统限制", error)
        }
    }
}
