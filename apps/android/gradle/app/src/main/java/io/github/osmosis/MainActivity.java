package io.github.osmosis;

import android.app.NativeActivity;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.view.View;

/**
 * 极薄的 NativeActivity 子类。所有 UI 都在 Rust(Slint)里;这个类只负责
 * 把窗口布局成全面屏(edge-to-edge),让 Slint UI 占据整个窗口,包括状态栏
 * 和导航栏底下的区域。Slint 的 Android 后端会自己读取这些安全区域并以
 * `safe-area-insets` 暴露给 UI,由 UI 自行留出内边距——所以这里不需要手动
 * 处理任何 inset。
 *
 * <p>另外还替 {@link MediaControls} 拿着当前的 Activity:运行期权限只能由
 * Activity 申请,而 Rust 那边握着的是 NativeActivity 的原生句柄。
 */
public class MainActivity extends NativeActivity {

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setupEdgeToEdge();
        MediaControls.attachActivity(this);
    }

    @Override
    protected void onDestroy() {
        MediaControls.detachActivity(this);
        super.onDestroy();
    }

    /**
     * 通知权限的结果。批了就记下来,下一次推送才会真的出通知 —— 申请是异步的,
     * 发起申请的那一次推送必然赶不上。
     */
    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(requestCode, permissions, results);
        for (int i = 0; i < permissions.length; i++) {
            if (android.Manifest.permission.POST_NOTIFICATIONS.equals(
                            permissions[i])
                    && results[i] == PackageManager.PERMISSION_GRANTED) {
                MediaControls.onNotificationPermissionGranted();
            }
        }
    }

    private void setupEdgeToEdge() {
        if (Build.VERSION.SDK_INT >= 30) {
            getWindow().setDecorFitsSystemWindows(false);
        } else {
            getWindow().getDecorView().setSystemUiVisibility(
                    View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                            | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                            | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
    }
}
