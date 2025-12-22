-- game

INSERT INTO rb_game (id, title, cover, is_shown, is_online, reg_open_at, pre_open_at, start_at, end_at)
VALUES (1, 'RBPH Test Game', 'https://goldenph.art/media/areas/intro/intro.webp', TRUE, TRUE,
        '2025-11-01 20:00:00+08', '2025-11-08 19:00:00+08', '2025-11-08 20:00:00+08', '2025-11-20 20:00:00+08');

SELECT setval('rb_game_id_seq', 100);

-- round

INSERT INTO rb_round (id, title, content, content_type, game_id)
VALUES (1, '序幕', '序幕', 0, 1);

SELECT setval('rb_round_id_seq', 100);

-- puzzle

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, unlock_cond, round_id)
VALUES (1, '序幕', 1, '提交「START」以开始游戏。', 0,
        '[{"type":"exact","text":"START","action":"start_game"},{"type":"exact","text":"EGG","action":"easter_egg"}]',
        'default', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, unlock_cond, round_id)
VALUES (2, '命名毋以讹传之', 1, E'<div class="text-center">\n\n*只有起错的名字，没有叫错的外号。*\n\n</div>\n\n![](https://info.pkupuzzle.art/assets/images/image_1-34a27cc0b5fab8a33eac4f01db91ee5d.webp)', 0,
        '[{"type":"exact","text":"ACRE CAMP","action":"milestone"},{"type":"exact","text":"ORME SHOE","action":"correct"},{"type":"exact","text":"ORME SHOE","action":"correct"}]',
        '', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, unlock_cond, round_id)
VALUES (3, '只说明书', 1, E'<div class="text-center">\n\n*不讲暗话，只说明书。*\n\n</div><hr>\n\n题目内容略。', 0,
        '[{"type":"exact","text":"UTOPIAHYMN","action":"milestone"},{"type":"exact","text":"LEISHMANIA","action":"milestone"},{"type":"exact","text":"MEMBERLESS","action":"milestone"},{"type":"exact","text":"DRAWSTRING","action":"milestone"},{"type":"exact","text":"THEREAFTER","action":"milestone"},{"type":"exact","text":"GLUTENFREE","action":"milestone"},{"type":"exact","text":"1099","action":"milestone","result":"本小题答案是【GLUTENFREE】"},{"type":"exact","text":"FLIPS","action":"milestone","result":"请将“填字游戏”当前图片沿长边翻转后，回到第 1 步重新完成题目。"},{"type":"exact","text":"WHIRL","action":"milestone","result":"时间在流逝……请将“填字游戏”当前图片按箭头方向旋转 90° 后，回到第 1 步重新完成题目。"},{"type":"exact","text":"BRUSH","action":"correct"}]',
        '', 1);

SELECT setval('rb_puzzle_id_seq', 100);

UPDATE rb_round SET puzzle = 1
WHERE id = 1;

-- currency

INSERT INTO rb_currency (id, cname, growth, max_amount, prec, game_id)
VALUES (1, '提示点', 1, 1000000, 0, 1);

-- hint

INSERT INTO rb_hint (id, sort, title, content, content_type, cooldown, cost_id, cost_amount, puzzle_id)
VALUES (1, 0, '我看不懂题目图中左上角写的那句话。', '这是提示内容。', 0, 0, 1, 100, 2);

INSERT INTO rb_hint (id, sort, title, content, content_type, cooldown, cost_id, cost_amount, puzzle_id)
VALUES (2, 1, '这是一个免费提示！', '但是是空的。', 0, 0, NULL, 0, 2);

INSERT INTO rb_hint (id, sort, title, content, content_type, cooldown, cost_id, cost_amount, puzzle_id)
VALUES (3, 2, '这道题的答案是什么？', '这道题的答案是【ORME SHOE】', 0, 7200, 1, 100, 2);

SELECT setval('rb_hint_id_seq', 100);

-- user

INSERT INTO rb_user (id, email, pass, urole, nickname)
VALUES (1, 'test@rabbi.town', '$2b$12$tu7u2NM5PFaFcs3F.ZykLe8F2olKRQYH8zSQK9hybJdDZta8Pmnd6', 1, 'user_1');

SELECT setval('rb_user_id_seq', 100);

-- team

INSERT INTO rb_team (id, tname, tstate, pass, bio, game_id)
VALUES (1, '蜡笔糖', 0, 'bili20fans', '', 1);

SELECT setval('rb_team_id_seq', 100);

INSERT INTO rb_team_member (team_id, user_id, game_id, is_captain)
VALUES (1, 1, 1, TRUE);

INSERT INTO rb_team_puzzle (team_id, puzzle_id, pstate)
VALUES (1, 1, 0);

INSERT INTO rb_team_puzzle (team_id, puzzle_id, pstate)
VALUES (1, 2, 0);

INSERT INTO rb_team_puzzle (team_id, puzzle_id, pstate)
VALUES (1, 3, 0);
