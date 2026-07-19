// 端口与 justfile 的 web_port 一致。改一处就得改另一处 —— 两边都写了理由。
//
// 单独一个模块,不放 playwright.config.ts 里:spec 从配置文件导入会形成循环,
// Playwright 会时灵时不灵地报「test() 不该在这里调用」,按加载顺序碰运气。
export const WEB_PORT = 8073;
