package io.github.slintstudy;

import android.app.NativeActivity;
import android.os.Build;
import android.os.Bundle;
import android.view.View;

/**
 * 极薄的 NativeActivity 子类。所有 UI 都在 Rust(Slint)里;这个类只负责
 * 把窗口布局成全面屏(edge-to-edge),让 Slint UI 占据整个窗口,包括状态栏
 * 和导航栏底下的区域。Slint 的 Android 后端会自己读取这些安全区域并以
 * `safe-area-insets` 暴露给 UI,由 UI 自行留出内边距——所以这里不需要手动
 * 处理任何 inset。
 */
public class MainActivity extends NativeActivity {

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setupEdgeToEdge();
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
