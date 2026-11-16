package com.meshshare.android.storage

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

/**
 * 接收文件默认写入应用私有目录；授予「所有文件访问权限」后改为写入
 * 手机公共的 Download/MeshShare，方便在文件管理器里找到。
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
            try {
                hostActivity.startActivity(
                    Intent(
                        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        Uri.parse("package:${context.packageName}"),
                    ),
                )
                invoke.resolve()
            } catch (error: Exception) {
                try {
                    hostActivity.startActivity(
                        Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION),
                    )
                    invoke.resolve()
                } catch (fallbackError: Exception) {
                    invoke.reject("无法打开存储授权页面", fallbackError)
                }
            }
        }
    }
}
