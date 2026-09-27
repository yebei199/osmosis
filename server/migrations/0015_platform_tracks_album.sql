-- 曲目带上所属专辑(#156),歌单分类视图按它分组。
--
-- 只存身份与名字,不另开专辑表:这里没有「专辑详情」的需求,分组只要名字,
-- 点进专辑要 id。两列都可空 —— 平台没给专辑(单曲、下架)就是 NULL;老行也是
-- NULL,等下一次刷新整体覆盖时补上。

ALTER TABLE platform_tracks
    ADD COLUMN album_id TEXT,
    ADD COLUMN album_name TEXT;
