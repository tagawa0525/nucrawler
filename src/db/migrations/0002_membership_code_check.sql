-- artifacts.input_scope は会員資格の code をソートして '+' でつないだもの（公開は 'public'）で、
-- 成果物の一意性の鍵に含まれる。表現が衝突しないよう、code は小文字英数字と '_' だけにし、
-- 'public' は予約する。また成果物側に code を複製して持つので、登録後の code の変更は禁止する。
-- 既存のテーブルに CHECK は足せないので、トリガーで検証する。

CREATE TRIGGER memberships_code_insert
BEFORE INSERT ON memberships
WHEN NEW.code = '' OR NEW.code = 'public' OR NEW.code GLOB '*[^a-z0-9_]*'
BEGIN
    SELECT RAISE(ABORT, 'invalid membership code: use [a-z0-9_] and not "public"');
END;

CREATE TRIGGER memberships_code_immutable
BEFORE UPDATE OF code ON memberships
WHEN NEW.code IS NOT OLD.code
BEGIN
    SELECT RAISE(ABORT, 'membership code cannot be changed once registered');
END;
