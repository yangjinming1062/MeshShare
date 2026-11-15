package com.meshshare.android.update

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import androidx.appcompat.app.AppCompatActivity
import androidx.lifecycle.Lifecycle
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.lang.ref.WeakReference

private const val LOG_TAG = "MeshShareUpdate"
private const val INSTALL_TIMEOUT_MS = 180_000L
private const val SESSION_CLEANUP_RETRY_MS = 5_000L

@InvokeArg
class ObserveArgs {
    lateinit var channel: Channel
}

@InvokeArg
class InstallArgs {
    lateinit var version: String
    var versionCode: Long = 0
}

@TauriPlugin
class UpdatePlugin(activity: Activity) : Plugin(activity) {
    private val context = activity.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val installer = context.packageManager.packageInstaller
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val packageName = context.packageName
    private val installed by lazy { readInstalledPackage() }
    private var resumedActivity = (activity as? AppCompatActivity)
        ?.takeIf { it.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED) }
    private var observer: Channel? = null
    private var activeInstall: Install? = null

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            emitEnvironment()
        }

        override fun onLost(network: Network) {
            emitEnvironment()
        }
    }

    init {
        // A process exit can leave a staged or unconfirmed session behind.
        installer.mySessions.filter { it.appPackageName == packageName }.forEach { session ->
            try {
                installer.abandonSession(session.sessionId)
            } catch (error: Exception) {
                Log.w(LOG_TAG, "Cannot discard previous install session ${session.sessionId}", error)
            }
        }
        activePlugin = WeakReference(this)
    }

    override fun onResume(activity: AppCompatActivity) {
        resumedActivity = activity
        emitEnvironment()
    }

    override fun onPause(activity: AppCompatActivity) {
        if (resumedActivity === activity) {
            resumedActivity = null
            emitEnvironment()
        }
    }

    @Command
    fun observe(invoke: Invoke) {
        val channel = invoke.parseArgs(ObserveArgs::class.java).channel
        main.post {
            try {
                if (observer == null) connectivity.registerDefaultNetworkCallback(networkCallback, main)
                observer = channel
                emitEnvironment()
                invoke.resolve()
            } catch (error: Exception) {
                invoke.reject("无法读取 Android 更新环境", error)
            }
        }
    }

    @Command
    fun openInstallSettings(invoke: Invoke) {
        main.post {
            try {
                val foreground = checkNotNull(resumedActivity) { "请先打开 MeshShare" }
                foreground.startActivity(
                    Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.fromParts("package", packageName, null)),
                )
                invoke.resolve()
            } catch (error: Exception) {
                invoke.reject("无法打开允许安装应用的系统设置", error)
            }
        }
    }

    @Command
    fun install(invoke: Invoke) {
        val args = invoke.parseArgs(InstallArgs::class.java)
        main.post {
            when {
                resumedActivity == null -> invoke.result("deferred")
                !context.packageManager.canRequestPackageInstalls() -> invoke.result("permission")
                activeInstall != null -> invoke.result("failed", "系统安装会话仍在进行")
                else -> {
                    val install = Install(invoke)
                    activeInstall = install
                    Thread({ stage(install, args) }, "meshshare-apk-stage").start()
                }
            }
        }
    }

    private fun emitEnvironment() {
        val channel = observer ?: return
        val capabilities = connectivity.getNetworkCapabilities(connectivity.activeNetwork)
        val network = when {
            capabilities?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) != true -> "offline"
            capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED) -> "unmetered"
            else -> "metered"
        }
        channel.send(JSObject().apply {
            put("foreground", resumedActivity != null)
            put("network", network)
            put("can_install", context.packageManager.canRequestPackageInstalls())
            put("version_code", installed.longVersionCode)
        })
    }

    @Suppress("DEPRECATION")
    private fun readInstalledPackage(): PackageInfo =
        context.packageManager.getPackageInfo(packageName, PackageManager.GET_SIGNING_CERTIFICATES)

    @Suppress("DEPRECATION")
    private fun verifyApk(apk: File, args: InstallArgs) {
        // Android 10 collects archive certificates only when GET_SIGNATURES is set.
        val candidate = context.packageManager.getPackageArchiveInfo(
            apk.absolutePath, PackageManager.GET_SIGNING_CERTIFICATES or PackageManager.GET_SIGNATURES,
        ) ?: throw InvalidApk("无法读取 APK 的包名和签名")
        if (candidate.packageName != packageName || candidate.versionName != args.version ||
            candidate.longVersionCode != args.versionCode || args.versionCode <= installed.longVersionCode
        ) {
            throw InvalidApk("APK 包名或版本与当前应用及更新清单不匹配")
        }
        val currentSigners = installed.signingInfo?.apkContentsSigners?.toSet().orEmpty()
        if (currentSigners.isEmpty() || currentSigners != candidate.signingInfo?.apkContentsSigners?.toSet()) {
            throw InvalidApk("APK 签名证书与已安装的 MeshShare 不匹配")
        }
    }

    private fun stage(install: Install, args: InstallArgs) {
        val apk = File(context.cacheDir, "android-update/update.apk")
        var sessionId: Int? = null
        try {
            check(apk.isFile) { "更新包已从缓存移除" }
            verifyApk(apk, args)
            val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
                setAppPackageName(packageName)
                setSize(apk.length())
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
                }
            }
            val id = installer.createSession(params)
            sessionId = id
            installer.openSession(id).use { session ->
                session.openWrite("base.apk", 0, apk.length()).use { output ->
                    apk.inputStream().use { it.copyTo(output) }
                    session.fsync(output)
                }
            }
            main.post { commit(install, id) }
        } catch (error: Exception) {
            main.post {
                install.sessionId = sessionId
                val kind = if (error is InvalidApk) "invalid_apk" else "failed"
                abort(install, kind, error.localizedMessage ?: "写入系统安装会话失败")
            }
        }
    }

    private fun commit(install: Install, sessionId: Int) {
        install.sessionId = sessionId
        if (resumedActivity == null) {
            abort(install, "deferred")
            return
        }
        if (!context.packageManager.canRequestPackageInstalls()) {
            abort(install, "permission")
            return
        }
        try {
            val intent = Intent(context, UpdateResultReceiver::class.java)
            val flags = PendingIntent.FLAG_CANCEL_CURRENT or
                (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0)
            val result = PendingIntent.getBroadcast(context, sessionId, intent, flags)
            install.result = result
            installer.openSession(sessionId).use { it.commit(result.intentSender) }
            main.postDelayed({ abort(install, "failed", "系统安装等待超时，请重试") }, INSTALL_TIMEOUT_MS)
        } catch (error: Exception) {
            abort(install, "failed", error.localizedMessage ?: "提交系统安装会话失败")
        }
    }

    internal fun onInstallResult(intent: Intent): Boolean {
        val sessionId = intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1)
        val install = activeInstall?.takeIf { it.sessionId == sessionId } ?: return false
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> confirm(install, intent)
            PackageInstaller.STATUS_SUCCESS -> finish(install, "installed")
            PackageInstaller.STATUS_FAILURE_ABORTED -> finish(install, "cancelled")
            PackageInstaller.STATUS_FAILURE_INVALID,
            PackageInstaller.STATUS_FAILURE_CONFLICT,
            PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> finish(install, "invalid_apk", statusMessage(intent))
            else -> finish(install, "failed", statusMessage(intent))
        }
        return true
    }

    @Suppress("DEPRECATION")
    private fun confirm(install: Install, intent: Intent) {
        if (install.confirmationStarted) return
        val foreground = resumedActivity
        if (foreground == null) {
            abort(install, "deferred")
            return
        }
        try {
            val confirmation = checkNotNull(intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)) {
                "系统未提供安装确认页面"
            }
            foreground.startActivity(confirmation)
            install.confirmationStarted = true
        } catch (error: Exception) {
            abort(install, "failed", error.localizedMessage ?: "无法打开系统安装确认页面")
        }
    }

    private fun abort(install: Install, kind: String, message: String? = null) {
        if (activeInstall !== install) return
        install.sessionId?.let { id ->
            try {
                installer.abandonSession(id)
            } catch (error: Exception) {
                if (installer.getSessionInfo(id) != null) {
                    // Keep the Rust transfer guard until Android settles the session.
                    Log.w(LOG_TAG, "Cannot abandon active install session $id", error)
                    main.postDelayed({ abort(install, kind, message) }, SESSION_CLEANUP_RETRY_MS)
                    return
                }
            }
        }
        finish(install, kind, message)
    }

    private fun finish(install: Install, kind: String, message: String? = null) {
        activeInstall = null
        install.result?.cancel()
        install.invoke.result(kind, message)
    }

    private fun statusMessage(intent: Intent): String =
        intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "Android 未能安装更新"

    private fun Invoke.result(kind: String, message: String? = null) {
        resolve(JSObject().apply {
            put("kind", kind)
            if (message != null) put("message", message)
        })
    }

    private class Install(val invoke: Invoke) {
        var sessionId: Int? = null
        var result: PendingIntent? = null
        var confirmationStarted = false
    }

    private class InvalidApk(message: String) : Exception(message)

    companion object {
        private var activePlugin: WeakReference<UpdatePlugin>? = null

        internal fun dispatchResult(intent: Intent): Boolean =
            activePlugin?.get()?.onInstallResult(intent) == true
    }
}

class UpdateResultReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (UpdatePlugin.dispatchResult(intent)) return
        if (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, 0) != PackageInstaller.STATUS_PENDING_USER_ACTION) return
        val id = intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1)
        val installer = context.packageManager.packageInstaller
        if (installer.getSessionInfo(id)?.appPackageName == context.packageName) {
            try {
                installer.abandonSession(id)
            } catch (error: Exception) {
                Log.w(LOG_TAG, "Cannot abandon confirmation for session $id", error)
            }
        }
    }
}
