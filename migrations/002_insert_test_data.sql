-- game

INSERT INTO rb_game (id, title, cover, is_shown, is_online, reg_open_at, pre_open_at, start_at, end_at)
VALUES (1, 'RBPH Test Game', '', TRUE, TRUE,
        '2025-11-01 20:00:00+08', '2025-11-08 19:00:00+08', '2025-11-08 20:00:00+08', '2025-11-20 20:00:00+08');

SELECT setval('rb_game_id_seq', 100);

-- round

INSERT INTO rb_round (id, title, content, content_type, game_id)
VALUES (1, '序幕', '提交「START」开始游戏。', 0, 1);

INSERT INTO rb_round (id, title, content, content_type, game_id)
VALUES (2, '最终谜题', '', 0, 1);

SELECT setval('rb_round_id_seq', 100);

-- puzzle

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, penalty, max_submit, unlock_cond, round_id)
VALUES (1, '序幕', 1, E'请提交「START」以开始游戏。\n\n**注意：**开始游戏后，不再能退出、解散队伍。', 0,
        '[{"type":"exact","text":"START","action":"start_game"},{"type":"exact","text":"bili20fans","action":"easter_egg","result":"我的 B 站 20 粉丝啦，哇！"}]',
        '[{"type":1,"args":[10]}]', NULL, 'default', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, penalty, max_submit, unlock_cond, round_id)
VALUES (2, '命名毋以讹传之', 1, E'<div class="text-center">\n\n*只有起错的名字，没有叫错的外号。*\n\n</div>\n\n题目内容略。', 0,
        '[{"type":"exact","text":"ORME SHOE","action":"correct"}]',
        '[{"type":2,"args":[10]}]', NULL, '(game-started)', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, penalty, max_submit, unlock_cond, round_id)
VALUES (3, '只说明书', 1, E'<div class="text-center">\n\n*不讲暗话，只说明书。*\n\n</div>\n\n题目内容略，请提交「MILESTONE」。', 0,
        '[{"type":"exact","text":"MILESTONE","action":"milestone","result":"本题答案是【BRUSH】"},{"type":"exact","text":"BRUSH","action":"correct"}]',
        '[{"type":1,"args":[10]},{"type":3,"args":[1,10]}]', 20, '(game-started)', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, penalty, max_submit, unlock_cond, round_id)
VALUES (4, '最终谜题', 1, E'<div class="text-center">\n\n*最终谜题*\n\n</div><hr>\n\n题目内容略。', 0,
        '[{"type":"exact","text":"FINAL ANSWER","action":"finish_game"}]',
        '[{"type":1,"args":[10]}]', 20, '(countge (set 2 3) 2)', 2);

SELECT setval('rb_puzzle_id_seq', 100);

UPDATE rb_round SET puzzle = 1
WHERE id = 1;

-- currency

INSERT INTO rb_currency (id, cname, growth, max_amount, prec, game_id)
VALUES (1, '提示点', 10, 1000000, 1, 1);

-- hint

INSERT INTO rb_hint (id, sort, title, content, content_type, cooldown, cost_id, cost_amount, puzzle_id)
VALUES (1, 0, '我看不懂题目图中左上角写的那句话。', '我也看不懂。', 0, 0, 1, 10, 2);

INSERT INTO rb_hint (id, sort, title, content, content_type, cooldown, cost_id, cost_amount, puzzle_id)
VALUES (2, 1, '这是一个免费提示！', '但是是空的。', 0, 0, NULL, 0, 2);

INSERT INTO rb_hint (id, sort, title, content, content_type, cooldown, cost_id, cost_amount, puzzle_id)
VALUES (3, 2, '这道题的答案是什么？', '这道题的答案是【ORME SHOE】', 0, 7200, 1, 100, 2);

SELECT setval('rb_hint_id_seq', 100);

-- user

-- test@rabbi.town : 12345678
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

INSERT INTO rb_team (id, tname, tstate, pass, bio, game_id, finish_at)
VALUES (2, '4S', 2, '1', '', 1, '2025-12-23 12:11');

INSERT INTO rb_team_puzzle (team_id, puzzle_id, pstate)
VALUES (2, 1, 0), (2, 2, 1), (2, 3, 1);

-- annoucement

INSERT INTO rb_announcement (title, content, content_type, is_pinned, is_shown, game_id, puzzle_id)
VALUES ('全站测试公告', '这是一条全站测试公告。', 0, TRUE, TRUE, NULL, NULL);

INSERT INTO rb_announcement (title, content, content_type, is_pinned, is_shown, game_id, puzzle_id)
VALUES ('比赛公告', '这是一条比赛公告。', 0, TRUE, TRUE, 1, NULL);

INSERT INTO rb_announcement (title, content, content_type, is_pinned, is_shown, game_id, puzzle_id)
VALUES ('「只说明书」题目更正', '请将 XXX 改为 XXX。', 0, FALSE, TRUE, 1, 3);
