package io.github.osmosis;

import android.content.ContentValues;
import android.content.Context;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.MediaStore;
import android.util.Log;

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
}
