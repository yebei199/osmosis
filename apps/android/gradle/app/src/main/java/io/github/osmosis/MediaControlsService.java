package io.github.osmosis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.IBinder;

/**
 * 挂着媒体会话与那条通知的前台服务。
 *
 * <p>与 Activity 同进程,所以<b>音频仍然在 Rust 那边出声</b> —— 这个服务不搬运
 * 任何声音,它是一张「别冻我」的凭据加一块显示区。没有它,切后台之后进程会被
 * 系统冻住,声音跟着停。
 *
 * <p>生命周期:第一次要出声时起(startForegroundService),暂停时降级为普通服务
 * 但留着通知(仍可控),停止时 stopSelf 并撤掉通知。Android 14 对长期挂着却不
 * 出声的 mediaPlayback 前台服务越来越不客气,所以暂停必须降级。
 */
public final class MediaControlsService extends Service {

    static final String ACTION_PUBLISH = "io.github.osmosis.PUBLISH";
    /** 通知上的按钮按下来时走这个 action,具体哪个键在 EXTRA_COMMAND 里。 */
    static final String ACTION_COMMAND = "io.github.osmosis.COMMAND";

    /** 通知上的按钮按的是哪个键。曲目信息不走 Intent,见 MediaControls.current。 */
    static final String EXTRA_COMMAND = "command";

    private static final String CHANNEL_ID = "media";
    /** 只有一条通知,固定编号即可。0 不能用 —— startForeground 不收。 */
    private static final int NOTIFICATION_ID = 1;

    private MediaSession session;
    private AudioManager audioManager;
    private AudioFocusRequest focusRequest;
    private BroadcastReceiver becomingNoisy;
    /** 已经拿到焦点了吗。丢了焦点要暂停,拿回来要继续,得知道自己在哪一边。 */
    private boolean holdsFocus;

    @Override
    public void onCreate() {
        super.onCreate();

        NotificationManager notifications =
                getSystemService(NotificationManager.class);
        // minSdk 26,渠道是必需品。IMPORTANCE_LOW:媒体卡片不该响也不该弹横幅。
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                getString(R.string.media_channel_name),
                NotificationManager.IMPORTANCE_LOW);
        channel.setDescription(getString(R.string.media_channel_description));
        channel.setShowBadge(false);
        notifications.createNotificationChannel(channel);

        session = new MediaSession(this, "osmosis");
        session.setCallback(new MediaSession.Callback() {
            @Override
            public void onPlay() {
                MediaControls.dispatch(MediaControls.COMMAND_PLAY, 0);
            }

            @Override
            public void onPause() {
                MediaControls.dispatch(MediaControls.COMMAND_PAUSE, 0);
            }

            @Override
            public void onStop() {
                MediaControls.dispatch(MediaControls.COMMAND_PAUSE, 0);
            }

            @Override
            public void onSkipToNext() {
                MediaControls.dispatch(MediaControls.COMMAND_NEXT, 0);
            }

            @Override
            public void onSkipToPrevious() {
                MediaControls.dispatch(MediaControls.COMMAND_PREVIOUS, 0);
            }

            @Override
            public void onSeekTo(long positionMs) {
                MediaControls.dispatch(
                        MediaControls.COMMAND_SEEK_TO, positionMs);
            }
        });
        session.setActive(true);

        audioManager = getSystemService(AudioManager.class);

        // 拔耳机、断蓝牙。不接这条的话声音会突然从外放公放出来 —— 这是
        // 用户唯一会当场后悔装了这个 app 的场景。
        becomingNoisy = new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                MediaControls.dispatch(MediaControls.COMMAND_PAUSE, 0);
            }
        };
        IntentFilter noisyFilter =
                new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            // targetSdk 34 起必须表态。这条是系统广播,只有系统发得出来。
            registerReceiver(
                    becomingNoisy, noisyFilter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(becomingNoisy, noisyFilter);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null) {
            return START_NOT_STICKY;
        }

        if (ACTION_COMMAND.equals(intent.getAction())) {
            // 通知上的键都不带参数(带参数的只有会话回调那条 onSeekTo)。
            MediaControls.dispatch(
                    intent.getIntExtra(EXTRA_COMMAND, MediaControls.COMMAND_TOGGLE),
                    0);
            return START_NOT_STICKY;
        }

        MediaControls.Snapshot now = MediaControls.current();
        boolean playing = now.status == MediaControls.STATUS_PLAYING;

        session.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, now.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, now.artists)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, now.durationMs)
                .putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, now.art)
                .build());

        // 位置只报这一次,通知上的进度条由系统按倍率自己往前推 —— 所以
        // 不必每秒推一遍。倍率为 0 就是「停在这」。
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY
                        | PlaybackState.ACTION_PAUSE
                        | PlaybackState.ACTION_PLAY_PAUSE
                        | PlaybackState.ACTION_SKIP_TO_NEXT
                        | PlaybackState.ACTION_SKIP_TO_PREVIOUS
                        | PlaybackState.ACTION_SEEK_TO)
                .setState(
                        playing
                                ? PlaybackState.STATE_PLAYING
                                : PlaybackState.STATE_PAUSED,
                        now.positionMs,
                        playing ? 1.0f : 0.0f)
                .build());

        updateFocus(playing);
        Notification notification = buildNotification(now, playing);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            // targetSdk 34 下没有这个类型参数会直接被系统拒掉。
            startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }

        if (!playing) {
            // 通知留着仍可控,服务降级 —— 见类注释。
            stopForeground(Service.STOP_FOREGROUND_DETACH);
        }

        return START_NOT_STICKY;
    }

    /** 拿焦点 / 还焦点。别的 app 开始放歌时我们要让路,来电同理。 */
    private void updateFocus(boolean playing) {
        if (playing && !holdsFocus) {
            focusRequest = new AudioFocusRequest.Builder(
                    AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build())
                    .setOnAudioFocusChangeListener(this::onFocusChange)
                    .build();
            holdsFocus = audioManager.requestAudioFocus(focusRequest)
                    == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
        } else if (!playing && holdsFocus && focusRequest != null) {
            audioManager.abandonAudioFocusRequest(focusRequest);
            holdsFocus = false;
        }
    }

    private void onFocusChange(int change) {
        switch (change) {
            case AudioManager.AUDIOFOCUS_LOSS:
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                // 短暂丢焦点(来电、导航播报)也暂停而不是压低音量:
                // 压低音量要有音量控制,而那条线还没接(见 #38 的「本次不做」)。
                MediaControls.dispatch(MediaControls.COMMAND_PAUSE, 0);
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                MediaControls.dispatch(MediaControls.COMMAND_PLAY, 0);
                break;
            default:
                break;
        }
    }

    private Notification buildNotification(
            MediaControls.Snapshot now, boolean playing) {
        Notification.Builder builder =
                new Notification.Builder(this, CHANNEL_ID)
                        .setSmallIcon(R.drawable.ic_media_notification)
                        .setContentTitle(now.title)
                        .setContentText(now.artists)
                        .setLargeIcon(now.art)
                        .setContentIntent(openApp())
                        .setOngoing(playing)
                        .setVisibility(Notification.VISIBILITY_PUBLIC)
                        .setStyle(new Notification.MediaStyle()
                                .setMediaSession(session.getSessionToken())
                                // 折叠态上限就是三个(平台定的),而这里正好三个键,
                                // 于是全放得下:展开与否看到的是同一排。
                                .setShowActionsInCompactView(0, 1, 2));

        builder.addAction(action(
                android.R.drawable.ic_media_previous,
                "上一首",
                MediaControls.COMMAND_PREVIOUS));
        builder.addAction(action(
                playing
                        ? android.R.drawable.ic_media_pause
                        : android.R.drawable.ic_media_play,
                playing ? "暂停" : "播放",
                MediaControls.COMMAND_TOGGLE));
        builder.addAction(action(
                android.R.drawable.ic_media_next,
                "下一首",
                MediaControls.COMMAND_NEXT));
        // 随机与循环**不上通知栏**,只在应用内设定(2026-08-13)。
        //
        // 它们曾经是第四、第五个键。折叠态最多显示三个(平台上限),于是这两个
        // 平时根本看不见,要展开才够得着 —— 而它们恰恰是**要先看见当前状态才按得对**
        // 的模式键,折叠态又不显示状态。锁屏上真正需要「不看也能按」的只有
        // 上一首/播放/下一首。桌面 MPRIS 那边照旧提供,那是完整控制面板,不是应急面板。

        return builder.build();
    }

    private Notification.Action action(int icon, String label, int command) {
        Intent intent = new Intent(this, MediaControlsService.class)
                .setAction(ACTION_COMMAND)
                .putExtra(EXTRA_COMMAND, command);
        PendingIntent pending = PendingIntent.getService(
                this,
                // 每个键要一个不同的 requestCode,否则后建的会顶掉先建的。
                command,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(
                android.graphics.drawable.Icon.createWithResource(this, icon),
                label,
                pending)
                .build();
    }

    /** 点通知本体回到应用。 */
    private PendingIntent openApp() {
        Intent intent = new Intent(this, MainActivity.class)
                .setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(
                this, 0, intent, PendingIntent.FLAG_IMMUTABLE);
    }

    @Override
    public void onDestroy() {
        if (becomingNoisy != null) {
            unregisterReceiver(becomingNoisy);
            becomingNoisy = null;
        }
        if (holdsFocus && focusRequest != null) {
            audioManager.abandonAudioFocusRequest(focusRequest);
            holdsFocus = false;
        }
        if (session != null) {
            session.setActive(false);
            session.release();
            session = null;
        }
        super.onDestroy();
    }

    /** 没有跨进程的客户端,不提供绑定。 */
    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
