apk := "dist/slint-study-debug.apk"

# USB 直装到手机(推荐:不受移动热点/公司 WiFi 客户端隔离影响)
install-apk:
    adb install -r {{apk}}

# 局域网 http 共享,手机扫码下载
# 可用前提:手机与电脑同一网络且无客户端隔离(如电脑自己开的热点)
# 连的是别人的移动热点/公司 WiFi 多半被隔离,手机连不上,请改用 install-apk
serve-apk:
    miniserve dist --interfaces 0.0.0.0 --port 3070 --qrcode
