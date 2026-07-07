package io.github.slintstudy;

import android.app.NativeActivity;
import android.os.Build;
import android.os.Bundle;
import android.view.View;

/**
 * Thin NativeActivity subclass. All UI lives in Rust (Slint); this class only
 * lays the window out edge-to-edge so the Slint UI owns the full window,
 * including the area behind the status and navigation bars. The Slint Android
 * backend reads the insets itself and exposes them as `safe-area-insets`, which
 * the UI pads around — so no manual inset plumbing is needed here.
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
