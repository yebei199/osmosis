package io.github.osmosis;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.os.Build;
import android.util.Log;

/**
 * 系统媒体控件的 Java 侧门面。
 *
 * <p>Rust 那边只跟这个类说话(见 apps/android/src/controls.rs),真正干活的是
 * {@link MediaControlsService} —— 锁屏与通知栏的卡片要活过切后台,就必须挂在
 * 一个前台服务上,而前台服务是 Java 的东西。理由见 docs/adr/0020。
 *
 * <p>所有东西都推到<b>第一次真的要出声</b>才发生:启动时申请通知权限,用户还
 * 不知道你要干什么;启动时挂一条常驻通知,没放歌也占着一行。两样都是招人删
 * app 的做法。
 */
public final class MediaControls {

    private static final String TAG = "osmosis";

    static {
        // NativeActivity 是用 dlopen 加载 libosmosis.so 的,而 dlopen 进来的库
        // **不会**登记到 Java 类加载器的库表里 —— JNI 解析 native 方法时查的正是
        // 那张表,于是 nativeCommand 会抛 UnsatisfiedLinkError,尽管符号确实在
        // .so 里(nm -D 看得到)。
        //
        // 这里补一次 System.loadLibrary:动态链接器认得已经加载的那份,不会真的
        // 再加载一遍,但 ART 会把它记进本类加载器的表里,解析这才成得了。
        //
        // 库名与 apps/android/Cargo.toml 的 [lib] name、以及 AndroidManifest 里
        // 的 android.app.lib_name 是同一个。
        System.loadLibrary("osmosis");
    }

    /** 与 Rust 侧 ui::MediaStatus 的顺序一一对应,改一边就要改另一边。 */
    public static final int STATUS_PLAYING = 0;
    public static final int STATUS_PAUSED = 1;
    public static final int STATUS_STOPPED = 2;

    /** 与 Rust 侧 ui::MediaCommand 的顺序一一对应,同上。 */
    public static final int COMMAND_PLAY = 0;
    public static final int COMMAND_PAUSE = 1;
    public static final int COMMAND_TOGGLE = 2;
    public static final int COMMAND_NEXT = 3;
    public static final int COMMAND_PREVIOUS = 4;
    public static final int COMMAND_SEEK_TO = 5;
    public static final int COMMAND_SEEK_BY = 6;
    // 7 与 8 曾是随机与循环,2026-08-13 从通知栏撤掉(见 MediaControlsService
    // 里那段理由),这一端不再发它们。Rust 侧 ui::MediaCommand 上仍有对应项 ——
    // 桌面 MPRIS 在用,所以序号留空不复用,免得两端对不上。

    /** 申请通知权限时用的请求码,只有这一处用它,取什么值都行。 */
    private static final int NOTIFICATION_PERMISSION_REQUEST = 0x05;

    /** 此刻在放什么。字段都是 final,整个对象换新而不是逐个改。 */
    static final class Snapshot {
        final int status;
        final String title;
        final String artists;
        final long durationMs;
        final long positionMs;
        final Bitmap art;

        Snapshot(
                int status,
                String title,
                String artists,
                long durationMs,
                long positionMs,
                Bitmap art) {
            this.status = status;
            this.title = title;
            this.artists = artists;
            this.durationMs = durationMs;
            this.positionMs = positionMs;
            this.art = art;
        }
    }

    /**
     * 最新的一份。写在 Rust 的线程上、读在主线程上,故 volatile。
     *
     * <p><b>刻意不走 Intent extra。</b>封面是 512×512 的 ARGB,正好一兆,而 Binder
     * 一次事务的上限也在一兆附近 —— 塞进 Intent 会在真机上偶发
     * TransactionTooLargeException,而且是「有的歌行、有的歌炸」那种。服务与
     * Activity 本来就同进程,静态字段是直的那条路。
     */
    private static volatile Snapshot current =
            new Snapshot(STATUS_STOPPED, null, null, 0, 0, null);

    /**
     * 当前的 Activity。申请运行期权限只能由 Activity 发起,而 Rust 那边拿到的
     * 是 NativeActivity 的句柄,不方便回头找。{@link MainActivity} 在
     * onCreate/onDestroy 里维护它。
     */
    private static volatile Activity activity;

    /** 权限被拒过一次就不再问。反复弹框比没有通知更烦人。 */
    private static volatile boolean notificationPermissionAsked;

    private MediaControls() {
    }

    static void attachActivity(Activity current) {
        activity = current;
    }

    static void detachActivity(Activity current) {
        if (activity == current) {
            activity = null;
        }
    }

    static Snapshot current() {
        return current;
    }

    /**
     * 报一次「现在在放什么」。第一次带着歌来会把服务拉起来。
     *
     * <p>由 Rust 侧在播放状态变化时调用,已经去过重了(见 ui::media 的 Bridge),
     * 所以这里不必再判一次一样不一样。
     *
     * @param status  {@link #STATUS_PLAYING} 之一
     * @param artists 已经拼好的一行,通知上本来就只有一行的位置
     * @param art     封面,可为 null
     */
    public static void publish(
            int status,
            String title,
            String artists,
            long durationMs,
            long positionMs,
            int[] argb,
            int artWidth,
            int artHeight) {
        // 封面以 ARGB_8888 的裸像素过来。通道重排在 Rust 侧做(那边拿到的是
        // RGBA),这里只负责把它包成 Bitmap —— 一行的事,不值得为它多写 JNI。
        Bitmap art = null;
        if (argb != null && argb.length == artWidth * artHeight
                && artWidth > 0 && artHeight > 0) {
            art = Bitmap.createBitmap(
                    argb, artWidth, artHeight, Bitmap.Config.ARGB_8888);
        }

        current = new Snapshot(
                status, title, artists, durationMs, positionMs, art);

        Activity host = activity;
        if (host == null) {
            // 界面还没起来或者已经没了,这时没有服务可推。
            return;
        }

        Intent intent = new Intent(host, MediaControlsService.class);
        if (status == STATUS_STOPPED) {
            host.stopService(intent);
            return;
        }

        ensureNotificationPermission(host);
        host.startForegroundService(
                intent.setAction(MediaControlsService.ACTION_PUBLISH));
    }

    /**
     * 第一次要出声时才申请通知权限。
     *
     * <p>API 33 起这是运行期权限;拒了就没有通知,也就没有锁屏控件,但
     * MediaSession 还在 —— 耳机线控与蓝牙按键照样能用。降级而不是罢工。
     *
     * <p>只问一次。反复弹框换不来一个「允许」,只换来一个卸载。
     */
    private static void ensureNotificationPermission(Activity host) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            return;
        }
        if (notificationPermissionAsked) {
            return;
        }
        if (host.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                == PackageManager.PERMISSION_GRANTED) {
            return;
        }

        notificationPermissionAsked = true;
        // 权限框只能从主线程弹,而这里多半跑在 Rust 那条线程上。
        host.runOnUiThread(() -> host.requestPermissions(
                new String[] {Manifest.permission.POST_NOTIFICATIONS},
                NOTIFICATION_PERMISSION_REQUEST));
    }

    /**
     * 用户批了权限。由 {@link MainActivity} 在权限结果回来时调用。
     *
     * <p>这里什么都不用做 —— 权限有了之后下一次推送自然就能出通知。留着这个
     * 方法是为了让「问过了」与「被拒了」在代码里分得开:重新申请那条路只有
     * 用户去系统设置里开,不由我们再弹一次。
     */
    static void onNotificationPermissionGranted() {
        Log.i(TAG, "通知权限已获准,下一次推送会出媒体卡片");
    }

    /**
     * 把外面按的键送回 Rust。
     *
     * <p>会话回调、通知上的按钮、音频焦点变化、拔耳机广播,四条路都从这里出去。
     *
     * @param argument 只有跳转用得上(毫秒),其余传 0
     */
    static void dispatch(int command, long argument) {
        try {
            nativeCommand(command, argument);
        } catch (UnsatisfiedLinkError missing) {
            // Rust 侧还没接上(见 #42)。缺了它只是按键没反应,不该把
            // 通知栏一起带崩。
            Log.w(TAG, "媒体控件的原生入口还没接上,这一下被丢掉了", missing);
        }
    }

    /**
     * Rust 侧的入口,由 apps/android/src/controls.rs 实现。
     *
     * <p>符号名与本类的包名、类名、方法名绑死。改任何一个都要同时改那边 ——
     * 链接期不会有人提醒,只会在按下按钮那一刻抛 UnsatisfiedLinkError。
     */
    private static native void nativeCommand(int command, long argument);
}
