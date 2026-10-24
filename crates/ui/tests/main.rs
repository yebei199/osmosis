//! 共用一个集成测试产物；nextest 为每条测试启动独立进程，隔离 Slint 后端状态。

mod banner;
mod controls;
mod download;
mod facets;
mod fonts;
mod group_capsule;
mod liked;
mod literals;
mod login;
mod lyrics_page;
mod music_list_layout;
mod music_page_highlight;
mod nav_glass;
mod navigation;
#[path = "player_bar/main.rs"]
mod player_bar;
mod player_bar_reserve;
mod playlist_edit;
mod playlists;
mod profile;
mod queue_page;
mod search;
mod settings_page;
mod status_strips;
mod theme;
mod thumbnail;
mod viz_anchor;
mod wall_view;
