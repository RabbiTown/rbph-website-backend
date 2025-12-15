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
VALUES (1, '序幕', 1, '测试测试测试', 0,
        '[{"type":"exact","text":"start","action":"start_game"},{"type":"exact","text":"egg","action":"easter_egg"}]',
        'default', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, unlock_cond, round_id)
VALUES (2, '序幕 2', 1, '测试测试测试 2', 0,
        '[{"type":"exact","text":"ACRE CAMP","action":"milestone"},{"type":"exact","text":"ORME SHOE","action":"correct"},{"type":"exact","text":"ORME SHOE","action":"correct"}]',
        '', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, unlock_cond, round_id)
VALUES (3, '序幕 3', 1, '测试测试测试 3', 0,
        '[{"type":"exact","text":"ACRE CAMP","action":"milestone"},{"type":"exact","text":"ORME SHOE","action":"correct"}]',
        '', 1);

SELECT setval('rb_puzzle_id_seq', 100);

UPDATE rb_round SET puzzle = 1
WHERE id = 1;

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
