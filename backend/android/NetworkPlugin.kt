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
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.net.InetAddress

private const val LOG_TAG = "MeshShareNetwork"
private const val LAN_BIND_RETRY_MS = 1_000L

@InvokeArg
class ObserveArgs {
    lateinit var channel: Channel
}

/**
 * 普通 VPN 成为进程默认网络时，把 MeshShare 的 socket 固定在物理局域网上。Rust 节点直接
 * 创建 TCP/UDP socket，只有进程级绑定才能同时覆盖 mdns-sd 创建的组播 socket。
 */
@TauriPlugin
class NetworkPlugin(activity: Activity) : Plugin(activity) {
    private val connectivity =
        activity.applicationContext.getSystemService(ConnectivityManager::class.java)
    private val wifi = activity.applicationContext.getSystemService(WifiManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private var destroyed = false
    private var boundNetworkHandle: Long? = null
    private var bindRetryPosted = false
    private var multicastLock: WifiManager.MulticastLock? = null
    private var observer: Channel? = null

    private val lanRequest = NetworkRequest.Builder()
        .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
        .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
        .addTransportType(NetworkCapabilities.TRANSPORT_ETHERNET)
        .build()

    private val defaultNetworkCallback = networkCallback()
    private val lanNetworkCallback = networkCallback()

    private fun networkCallback() = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = scheduleRebind()

        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) =
            scheduleRebind()

        override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) =
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
        // 这里同步执行，Rust 监听器和 mDNS 守护进程会在绑定生效后才创建。
        rebindProcessNetwork()
    }

    @Command
    fun observe(invoke: Invoke) {
        val channel = invoke.parseArgs(ObserveArgs::class.java).channel
        main.post {
            if (destroyed) {
                invoke.reject("网络监听已停止")
                return@post
            }
            try {
                if (observer?.id != channel.id) {
                    // 通知节点重新读取注册期间可能已经变化的绑定。
                    channel.send(JSObject())
                    observer = channel
                }
                invoke.resolve()
            } catch (error: Exception) {
                invoke.reject("无法登记网络绑定监听", error)
            }
        }
    }

    override fun onDestroy(activity: androidx.appcompat.app.AppCompatActivity) {
        destroyed = true
        observer = null
        try {
            connectivity.unregisterNetworkCallback(defaultNetworkCallback)
        } catch (_: Exception) {
            // 部分设备或系统配置会拒绝注册网络回调，注销失败可以忽略。
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
        if (destroyed) return
        // 每次网络事件都允许重新尝试一次绑定，重试自身不再排新的重试。
        bindRetryPosted = false
        main.post { if (!destroyed) rebindProcessNetwork() }
    }

    private fun rebindProcessNetwork() {
        val lan = selectLanNetwork()
        val target = lan?.takeIf { vpnCapturesLan(it) }
        val targetHandle = target?.networkHandle
        if (targetHandle != boundNetworkHandle) {
            val previousHandle = boundNetworkHandle
            val bound = connectivity.bindProcessToNetwork(target)
            if (bound && processNetworkHandle() == targetHandle) {
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
                scheduleBindRetry(targetHandle)
            }
            if (boundNetworkHandle != previousHandle) {
                observer?.send(JSObject())
            }
        }
    }

    private fun processNetworkHandle(): Long? =
        try {
            connectivity.getBoundNetworkForProcess()?.networkHandle
        } catch (error: Exception) {
            Log.w(LOG_TAG, "读取 MeshShare 网络绑定失败", error)
            null
        }

    /** 系统可能忽略切换瞬间的绑定，每个网络事件至多重试一次，避免进程永久留在隧道网络里。 */
    private fun scheduleBindRetry(targetHandle: Long?) {
        if (destroyed || bindRetryPosted || targetHandle == null) return
        bindRetryPosted = true
        main.postDelayed({ if (!destroyed) rebindProcessNetwork() }, LAN_BIND_RETRY_MS)
    }

    /**
     * 只有默认网络是普通 VPN、且它的路由会收走发往本机局域网地址的流量时，才需要把进程切到
     * 物理局域网。代理类 VPN 常用 0.0.0.0/1 与 128.0.0.0/1 取代默认路由，只看前缀长度会把
     * 这种全隧道误判成分流，因此按路由是否覆盖局域网地址判断。
     */
    private fun vpnCapturesLan(lan: Network): Boolean {
        val active = connectivity.activeNetwork ?: return false
        if (active.networkHandle == lan.networkHandle) return false
        val capabilities = connectivity.getNetworkCapabilities(active) ?: return false
        if (!capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) {
            Log.i(LOG_TAG, "默认网络不是 VPN，保持系统网络选择")
            return false
        }
        val vpn = connectivity.getLinkProperties(active) ?: return true
        if (vpn.isTailscaleNetwork()) {
            Log.i(LOG_TAG, "检测到 Tailscale 组网，保留 Tailscale 网络")
            return false
        }
        // 读不到路由或局域网地址时按保守策略绑定，避免整个进程留在隧道网络里。
        if (vpn.routes.isEmpty()) return true
        val lanAddresses = connectivity.getLinkProperties(lan)
            ?.linkAddresses
            ?.map { address -> address.address }
            .orEmpty()
        if (lanAddresses.isEmpty()) return true
        val captures = vpn.routes.any { route ->
            lanAddresses.any { address -> route.destination.contains(address) }
        }
        if (!captures) Log.i(LOG_TAG, "VPN 路由不覆盖局域网地址，保持系统网络选择")
        return captures
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
