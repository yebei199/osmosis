# 局域网共享 apk 给手机下载(miniserve 是 Rust 写的静态服务器,自带二维码)
serve-apk:
    miniserve dist --interfaces 0.0.0.0 --port 8080 --qrcode
