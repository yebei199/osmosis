package io.github.osmosis;

import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.os.Build;
import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * 应用内升级的安装一侧(#129):把 Rust 下好并核对过的 APK 交给系统安装器。
 *
 * <p>走 PackageInstaller 会话而不是 {@code ACTION_VIEW} + FileProvider:后者要 androidx,
 * 本应用不引任何 androidx。系统收下会话后发回 {@code STATUS_PENDING_USER_ACTION},
 * 这里再把它给的确认界面拉起来 —— 首次还会先要求「安装未知应用」的许可。
 * 签名一致时是覆盖安装,应用私有目录里的登录态原样留着。
 */
public final class Updater extends BroadcastReceiver {

    private static final String TAG = "osmosis";

    /**
     * 把 {@code path} 写进一个安装会话并提交。文件拷进会话之后就删掉,
     * 不论成败 —— 留着就是一百多 MB,下一次反正会重下。
     *
     * @return 交出去了没有。装不装由用户在系统界面上定,结果回到 {@link #onReceive}。
     */
    public static boolean install(String path) {
        Context context = MediaControls.appContext();
        File apk = new File(path);
        if (context == null) {
            Log.w(TAG, "还没有 Context,装不了");
            apk.delete();
            return false;
        }

        try {
            PackageInstaller installer =
                    context.getPackageManager().getPackageInstaller();
            PackageInstaller.SessionParams params =
                    new PackageInstaller.SessionParams(
                            PackageInstaller.SessionParams.MODE_FULL_INSTALL);
            params.setAppPackageName(context.getPackageName());
            int id = installer.createSession(params);
            try (PackageInstaller.Session session = installer.openSession(id)) {
                try (InputStream in = new FileInputStream(apk);
                        OutputStream out =
                                session.openWrite("base.apk", 0, apk.length())) {
                    byte[] buffer = new byte[1 << 16];
                    int n;
                    while ((n = in.read(buffer)) > 0) {
                        out.write(buffer, 0, n);
                    }
                    session.fsync(out);
                }
                // MUTABLE 是必须的:安装状态由系统填进这个 Intent 的 extras。
                // 显式指向本类,所以可变也不会被别的应用截走。
                PendingIntent status =
                        PendingIntent.getBroadcast(
                                context,
                                id,
                                new Intent(context, Updater.class),
                                PendingIntent.FLAG_UPDATE_CURRENT
                                        | PendingIntent.FLAG_MUTABLE);
                session.commit(status.getIntentSender());
            }
            return true;
        } catch (Exception e) {
            Log.w(TAG, "交不给系统安装器", e);
            return false;
        } finally {
            apk.delete();
        }
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        int status =
                intent.getIntExtra(
                        PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Intent confirm =
                    Build.VERSION.SDK_INT >= 33
                            ? intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent.class)
                            : legacyConfirm(intent);
            if (confirm != null) {
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                context.startActivity(confirm);
            }
            return;
        }
        if (status != PackageInstaller.STATUS_SUCCESS) {
            Log.w(
                    TAG,
                    "升级没装上: "
                            + status
                            + " "
                            + intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE));
        }
    }

    @SuppressWarnings("deprecation")
    private static Intent legacyConfirm(Intent intent) {
        return intent.getParcelableExtra(Intent.EXTRA_INTENT);
    }
}
