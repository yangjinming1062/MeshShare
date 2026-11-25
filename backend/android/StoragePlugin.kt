package com.meshshare.android.storage

import android.app.Activity
import android.app.DownloadManager
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.DocumentsContract
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.io.File
import java.io.IOException

/**
 * 接收文件默认使用应用私有目录。获得「所有文件访问权限」时，
 * 接收文件保存到公共的 Download/MeshShare。
 */
@TauriPlugin
class StoragePlugin(activity: Activity) : Plugin(activity) {
    private val hostActivity = activity
    private val context = activity.applicationContext
    private val main = Handler(Looper.getMainLooper())

    @Command
    fun requestAllFilesAccess(invoke: Invoke) {
        main.post {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                invoke.resolve()
                return@post
            }
            openActivity(
                invoke,
                Intent(
                    Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                    Uri.parse("package:${context.packageName}"),
                ),
                Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION),
                "无法打开存储授权页面",
            )
        }
    }

    /** 在系统文件管理器里定位公共目录。应用专属目录也在主存储卷上，须单独判断。 */
    @Command
    fun revealPath(invoke: Invoke) {
        main.post {
            val path = invoke.getArgs().getString("path", null)
            if (path.isNullOrEmpty()) {
                invoke.reject("缺少目录路径")
                return@post
            }
            val uri = try {
                val directory = File(path).canonicalFile.toPath()
                val appFiles = context.getExternalFilesDir(null)?.canonicalFile?.toPath()
                if (appFiles != null && directory.startsWith(appFiles)) {
                    invoke.reject("文件保存在应用专属目录。授予「所有文件访问」后，新接收的文件会保存到公共下载目录")
                    return@post
                }
                @Suppress("DEPRECATION")
                val primary = Environment.getExternalStorageDirectory().canonicalFile.toPath()
                if (directory == primary || !directory.startsWith(primary)) {
                    invoke.reject("系统文件管理器只能定位主存储中的公共目录")
                    return@post
                }
                val relative = primary.relativize(directory)
                DocumentsContract.buildDocumentUri(
                    "com.android.externalstorage.documents",
                    "primary:$relative",
                )
            } catch (error: IOException) {
                invoke.reject("无法解析接收目录", error)
                return@post
            }
            val view = Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, "vnd.android.document/directory")
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            openActivity(
                invoke,
                view,
                Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                "无法打开所在目录：$path",
            )
        }
    }

    private fun openActivity(invoke: Invoke, preferred: Intent, fallback: Intent, message: String) {
        try {
            try {
                hostActivity.startActivity(preferred)
            } catch (_: Exception) {
                hostActivity.startActivity(fallback)
            }
            invoke.resolve()
        } catch (error: Exception) {
            invoke.reject(message, error)
        }
    }
}
