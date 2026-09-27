-- PDF の本文抽出に対応したので、「未対応」として断念していた抽出を再試行の対象に戻す。
DELETE FROM stage_errors
WHERE stage = 'extract' AND last_error LIKE 'PDF is not supported yet%';
