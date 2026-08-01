-- 账号与会话。见 docs/adr/0017。
--
-- 账号只管自家数据的归属与同步;网易云的登录是账号名下的一个绑定,凭据本身
-- 存在 bang-dream 那侧,按这里的 id 分片(见那个仓库的 docs/adr/0009)。

CREATE TABLE accounts (
    id BIGSERIAL PRIMARY KEY,
    -- 大小写不敏感的唯一:注册 Alice 之后 alice 不该是另一个人
    username TEXT NOT NULL,
    -- argon2 的 PHC 串,自带盐与参数,不需要另一列
    password_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX accounts_username_key ON accounts (lower(username));

CREATE TABLE sessions (
    -- token 的 sha256,十六进制。明文不落库:库泄露不该等于会话被接管
    token_hash TEXT PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 登出与"这个账号的会话"都按账号查
CREATE INDEX sessions_account_id_idx ON sessions (account_id);
