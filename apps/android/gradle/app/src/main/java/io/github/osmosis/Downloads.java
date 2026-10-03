package io.github.osmosis;

import android.Manifest;
import android.app.Activity;
import android.app.RecoverableSecurityException;
import android.content.ContentUris;
import android.content.IntentSender;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.os.Build;
import android.content.ContentValues;
import android.content.Context;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.MediaStore;
import android.util.Log;

import java.util.ArrayList;
import java.util.ArrayDeque;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.HashMap;
import java.util.Map;

/**
 * 下载落盘的 Java 侧:把文件写进系统的公共「音乐」目录。
 *
 * <p>Rust 那边只跟这个类说话(见 apps/android/src/downloads.rs)。走 MediaStore
 * 而不是自己拼一条 /sdcard 路径:Android 10 起公共目录对应用不可直接写,而
 * MediaStore 插入自家的条目<b>不需要任何存储权限</b>,插完系统还会自己把它扫进
 * 音乐库 —— 那正是「别的播放器也看得到」的全部要求。
 *
 * <p>条目先以 {@code IS_PENDING=1} 建出来,写完才清掉那一位。中途断网时
 * {@link #finish} 收到 {@code keep=false},整条记录连同文件一起删掉 ——
 * 没有这一步,音乐库里会留下一首放到一半就停的歌,而它看起来跟正常的一样。
 */
public final class Downloads {

    private static final String TAG = "osmosis";

    /** 文件落在公共音乐目录下的这一层。与 Rust 侧报给用户的那句话对应。 */
    private static final String RELATIVE_PATH = "Music/osmosis/";

    /** 还没收尾的条目。令牌自增,不用 fd 当键 —— fd 关掉之后会被系统重用。 */
    private static final Map<Long, Uri> PENDING = new HashMap<>();

    private static long nextToken = 1;

    private Downloads() {}

    /**
     * 建一个待定条目。
     *
     * @return 令牌,{@code 0} 表示没建成(没有 Context、或者 MediaStore 拒绝)。
     */
    public static synchronized long open(String fileName) {
        Context context = MediaControls.appContext();
        if (context == null) {
            Log.w(TAG, "还没有 Context,下载没处放");
            return 0;
        }

        ContentValues values = new ContentValues();
        values.put(MediaStore.Audio.Media.DISPLAY_NAME, fileName);
        values.put(MediaStore.Audio.Media.MIME_TYPE, "audio/mpeg");
        values.put(MediaStore.Audio.Media.RELATIVE_PATH, RELATIVE_PATH);
        values.put(MediaStore.Audio.Media.IS_PENDING, 1);

        try {
            Uri uri =
                    context.getContentResolver()
                            .insert(
                                    MediaStore.Audio.Media.EXTERNAL_CONTENT_URI,
                                    values);
            if (uri == null) {
                Log.w(TAG, "MediaStore 没给出条目");
                return 0;
            }
            long token = nextToken++;
            PENDING.put(token, uri);
            return token;
        } catch (Exception e) {
            Log.w(TAG, "建不出待定条目", e);
            return 0;
        }
    }

    /**
     * 取走这个条目的可写文件描述符。<b>调用方负责关它</b> —— detach 之后
     * Java 这边不再持有它。
     *
     * @return 文件描述符,{@code -1} 表示取不到。
     */
    public static synchronized int detachFd(long token) {
        Context context = MediaControls.appContext();
        Uri uri = PENDING.get(token);
        if (context == null || uri == null) {
            return -1;
        }

        try {
            ParcelFileDescriptor pfd =
                    context.getContentResolver().openFileDescriptor(uri, "w");
            if (pfd == null) {
                return -1;
            }
            return pfd.detachFd();
        } catch (Exception e) {
            Log.w(TAG, "开不了写句柄", e);
            return -1;
        }
    }

    /**
     * 收尾。{@code keep} 为真就清掉 IS_PENDING(文件对音乐库与文件管理器可见),
     * 为假就把整条记录连同文件删掉。
     *
     * @return 成功与否。令牌不认识时为假 —— 那说明有人收了两次。
     */
    public static synchronized boolean finish(long token, boolean keep) {
        Context context = MediaControls.appContext();
        Uri uri = PENDING.remove(token);
        if (context == null || uri == null) {
            return false;
        }

        try {
            if (keep) {
                ContentValues values = new ContentValues();
                values.put(MediaStore.Audio.Media.IS_PENDING, 0);
                context.getContentResolver().update(uri, values, null, null);
            } else {
                context.getContentResolver().delete(uri, null, null);
            }
            return true;
        } catch (Exception e) {
            Log.w(TAG, "收尾失败", e);
            return false;
        }
    }
    /** 运行期授权的 Activity，不用于长任务的文件 Context。 */
    private static Activity host;
    /** 每个安装进程仅第一次进入请求读取权限。 */
    private static boolean readAsked;
    /** 系统权限框未返回期间持续刷新目录。 */
    private static boolean readPending;
    /** 与通知权限请求编号分开。 */
    static final int READ_REQUEST = 17801;
    /** 系统删除请求唯一在飞编号。 */
    static final int DELETE_REQUEST = 17802;
    /** 结果由 Rust 轮询消费，取消也必须有明确结果。 */
    private static Removal removal;

    /** Activity 重建时换掉句柄，文件结果仍留在进程内。 */
    static synchronized void attachActivity(Activity activity) { host = activity; }

    /** 旧 Activity 退出不能清掉新 Activity。 */
    static synchronized void detachActivity(Activity activity) {
        if (host == activity) { host = null; }
    }

    /** 只申请音乐读取权限，版本分支不带无关图片或视频权限。 */
    private static String readPermission() {
        return Build.VERSION.SDK_INT >= 33
                ? Manifest.permission.READ_MEDIA_AUDIO : Manifest.permission.READ_EXTERNAL_STORAGE;
    }

    /** 授权状态实时读系统，设置页外部授予后刷新即可看到旧文件。 */
    private static boolean canRead(Context context) {
        return context.checkSelfPermission(readPermission()) == PackageManager.PERMISSION_GRANTED;
    }

    /** 第一次进入请求；拒绝后仍能管理本次安装拥有的歌曲。 */
    public static synchronized void requestAccess() {
        Activity activity = host;
        if (activity == null) { throw new IllegalStateException("音乐授权没有Activity"); }
        if (readAsked || canRead(activity)) { return; }
        readAsked = true;
        readPending = true;
        activity.runOnUiThread(() -> {
            try { activity.requestPermissions(new String[] {readPermission()}, READ_REQUEST); }
            catch (RuntimeException error) {
                synchronized (Downloads.class) { readPending = false; }
                Log.w(TAG, "读取音乐授权无法发起", error);
            }
        });
    }

    /** Activity 回传后停止 pending 刷新；查询自行读取当前权限。 */
    static synchronized void onReadResult() { readPending = false; }

    /** 每条记录打包为 id、文件名、字节数、修改秒数，字段不靠分隔符解析。 */
    private static Map<String, String[]> query(Context context) {
        Map<String, String[]> entries = new LinkedHashMap<>();
        String[] columns = {MediaStore.Audio.Media._ID, MediaStore.Audio.Media.DISPLAY_NAME,
                MediaStore.Audio.Media.SIZE, MediaStore.Audio.Media.DATE_MODIFIED};
        String selection = MediaStore.Audio.Media.RELATIVE_PATH + "=? AND "
                + MediaStore.Audio.Media.IS_PENDING + "=0";
        try (Cursor cursor = context.getContentResolver().query(
                MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, columns, selection,
                new String[] {RELATIVE_PATH}, MediaStore.Audio.Media.DATE_MODIFIED + " DESC")) {
            if (cursor == null) { throw new IllegalStateException("MediaStore没有返回目录"); }
            while (cursor.moveToNext()) {
                String name = cursor.getString(1);
                if (name == null || name.startsWith(".") || !name.endsWith(".mp3")) { continue; }
                String id = Long.toString(cursor.getLong(0));
                entries.put(id, new String[] {id, name, Long.toString(cursor.getLong(2)), Long.toString(cursor.getLong(3))});
            }
        }
        return entries;
    }

    /** 未获读取权限时 MediaStore 自动仅暴露本次安装拥有的媒体。 */
    public static synchronized String[] list() {
        Context context = MediaControls.appContext();
        if (context == null) { throw new IllegalStateException("没有音乐目录Context"); }
        List<String> values = new ArrayList<>();
        values.add(canRead(context) ? "" : "未授权读取音乐，仅显示本次安装下载的歌曲；授权后可管理更早的文件");
        values.add(Boolean.toString(readPending));
        for (String[] entry : query(context).values()) { java.util.Collections.addAll(values, entry); }
        return values.toArray(new String[0]);
    }

    /** 一次系统删除的原始快照、实际结果和授权状态。 */
    private static final class Removal {
        /** 当前安装可直接删的也按实际完成项计数。 */
        final List<String[]> deleted = new ArrayList<>();
        /** 失败原因不把未删除条目算成功。 */
        final List<String> failures = new ArrayList<>();
        /** Android10逐条请求；11及以上一次提交整批。 */
        final ArrayDeque<String[]> remaining = new ArrayDeque<>();
        /** 用户取消与空删除分别表达。 */
        boolean cancelled;
        /** 系统确认还没回来。 */
        boolean pending;
    }

    /** 只认最新受限目录快照中的标识，拒绝任意Uri和目录外ID。 */
    public static synchronized String[] delete(String requested) {
        if (removal != null && removal.pending) { throw new IllegalStateException("系统删除确认尚未结束"); }
        Context context = MediaControls.appContext();
        if (context == null) { throw new IllegalStateException("没有音乐目录Context"); }
        removal = new Removal();
        Map<String, String[]> available = query(context);
        java.util.HashSet<String> seen = new java.util.HashSet<>();
        for (String id : requested.split("\n")) {
            if (id.isEmpty() || !seen.add(id)) { continue; }
            String[] entry = available.get(id);
            if (entry == null) { removal.failures.add(id + ": 文件不在已下载目录中"); }
            else { removal.remaining.add(entry); }
        }
        advance(context, removal);
        return result();
    }

    /** 直接删除自有项，旧所有权项才进入系统确认。 */
    private static void advance(Context context, Removal operation) {
        while (!operation.remaining.isEmpty()) {
            String[] entry = operation.remaining.peek();
            Uri uri = ContentUris.withAppendedId(MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, Long.parseLong(entry[0]));
            if (!query(context).containsKey(entry[0])) {
                operation.failures.add(entry[1] + ": 文件已移出下载目录或不可读取");
                operation.remaining.remove();
                continue;
            }
            try {
                int count = context.getContentResolver().delete(uri, null, null);
                if (count > 0) { operation.deleted.add(entry); }
                else { operation.failures.add(entry[1] + ": 系统未删除文件"); }
                operation.remaining.remove();
            } catch (SecurityException error) {
                IntentSender sender;
                if (Build.VERSION.SDK_INT >= 30) {
                    List<Uri> uris = new ArrayList<>();
                    Map<String, String[]> current = query(context);
                    operation.remaining.removeIf(item -> {
                        if (current.containsKey(item[0])) { return false; }
                        operation.failures.add(item[1] + ": 文件已移出下载目录");
                        return true;
                    });
                    for (String[] item : operation.remaining) {
                        uris.add(ContentUris.withAppendedId(MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, Long.parseLong(item[0])));
                    }
                    if (uris.isEmpty()) { continue; }
                    try { sender = MediaStore.createDeleteRequest(context.getContentResolver(), uris).getIntentSender(); }
                    catch (RuntimeException requestError) {
                        operation.failures.add(requestError.toString()); operation.remaining.clear(); return;
                    }
                } else if (error instanceof RecoverableSecurityException) {
                    sender = ((RecoverableSecurityException) error).getUserAction().getActionIntent().getIntentSender();
                } else {
                    operation.failures.add(entry[1] + ": " + error.getMessage());
                    operation.remaining.remove(); continue;
                }
                launchDelete(sender, operation);
                return;
            } catch (RuntimeException error) {
                operation.failures.add(entry[1] + ": " + error.getMessage());
                operation.remaining.remove();
            }
        }
    }

    /** 弹框必须在Activity主线程；发起失败也回传终态。 */
    private static void launchDelete(IntentSender sender, Removal operation) {
        Activity activity = host;
        if (activity == null) { operation.failures.add("没有Activity，无法确认删除"); operation.remaining.clear(); return; }
        operation.pending = true;
        activity.runOnUiThread(() -> {
            try { activity.startIntentSenderForResult(sender, DELETE_REQUEST, null, 0, 0, 0); }
            catch (IntentSender.SendIntentException | RuntimeException error) {
                synchronized (Downloads.class) {
                    operation.pending = false;
                    operation.failures.add("系统删除确认无法发起: " + error.getMessage());
                    operation.remaining.clear();
                }
                Log.w(TAG, "系统删除确认无法发起", error);
            }
        });
    }

    /** 11+成功由系统执行删除；10授权后重试当前文件并继续后续条目。 */
    static synchronized void onDeleteResult(int code) {
        if (removal == null || !removal.pending) { return; }
        removal.pending = false;
        if (code != Activity.RESULT_OK) {
            removal.cancelled = true;
            for (String[] entry : removal.remaining) { removal.failures.add(entry[1] + ": 已取消系统确认"); }
            removal.remaining.clear(); return;
        }
        Context context = MediaControls.appContext();
        if (context == null) { removal.failures.add("确认返回后音乐Context已失效"); removal.remaining.clear(); return; }
        try {
            if (Build.VERSION.SDK_INT >= 30) {
                Map<String, String[]> current = query(context);
                for (String[] entry : removal.remaining) {
                    if (!current.containsKey(entry[0])) { removal.deleted.add(entry); }
                    else { removal.failures.add(entry[1] + ": 系统确认后文件仍在"); }
                }
                removal.remaining.clear();
            } else { advance(context, removal); }
        } catch (RuntimeException error) {
            removal.failures.add(error.toString()); removal.remaining.clear();
        }
    }

    /** 空数组代表尚未返回；终态只交付一次。 */
    public static synchronized String[] pollDelete() {
        if (removal == null || removal.pending) { return new String[0]; }
        String[] result = result(); removal = null; return result;
    }

    /** 状态、失败数量、失败原因，之后才是实际删除条目四字段。 */
    private static String[] result() {
        List<String> values = new ArrayList<>();
        values.add(removal.pending ? "pending" : removal.cancelled ? "cancelled" : "complete");
        values.add(Integer.toString(removal.failures.size()));
        values.addAll(removal.failures);
        for (String[] entry : removal.deleted) { java.util.Collections.addAll(values, entry); }
        String[] result = values.toArray(new String[0]);
        // 同步结果已被delete调用消费，poll不再重复报告。
        if (!removal.pending) { removal = null; }
        return result;
    }

}
