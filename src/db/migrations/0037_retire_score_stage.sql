-- LLM の採点をやめた（計画 017）。採点のステージ（`score:<利用者>:<プロファイル>:v<版>`）の失敗と作業の予約は、
-- 消すステージが無くなり `status` に残り続けるので消す。保存済みの点数（scores）は `eval` の比較材料として残す。
DELETE FROM stage_errors WHERE stage LIKE 'score:%';
DELETE FROM work_claims WHERE stage LIKE 'score:%';
