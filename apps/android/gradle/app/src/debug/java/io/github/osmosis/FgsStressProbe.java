package io.github.osmosis;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

/**
 * 只在 debug 包里的探针(#150):在主线程上把「起前台服务 → 立刻停」连发 N 轮。
 *
 * <p>主线程被这个循环占着,服务的 onStartCommand 一次也轮不到,于是每一次停都必然追在
 * 还没调 startForeground 的那次启动后面 —— 这是媒体控件推送能碰到的最坏序列。门面没
 * 挡住的话,系统当场以 ForegroundServiceDidNotStartInTimeException 杀进程。
 *
 * <pre>adb shell am broadcast -n io.github.osmosis/.FgsStressProbe --ei rounds 50</pre>
 */
public final class FgsStressProbe extends BroadcastReceiver {

    @Override
    public void onReceive(Context context, Intent intent) {
        int rounds = intent.getIntExtra("rounds", 20);
        for (int i = 0; i < rounds; i++) {
            MediaControls.publish(MediaControls.STATUS_PLAYING,
                    "probe", "probe", 0, 0, null, 0, 0);
            MediaControls.publish(MediaControls.STATUS_STOPPED,
                    null, null, 0, 0, null, 0, 0);
        }
        Log.i("osmosis", "FgsStressProbe: " + rounds + " 轮起停已发完");
    }
}
