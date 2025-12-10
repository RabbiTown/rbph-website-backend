INSERT INTO rb_game (id, title, is_shown, is_online, reg_open_at, pre_open_at, start_at, end_at, cover)
VALUES (1, 'RBPH Test Game', TRUE, TRUE,
        '2025-11-01 20:00:00+08', '2025-11-08 19:00:00+08', '2025-11-08 20:00:00+08', '2025-11-20 20:00:00+08',
        'https://goldenph.art/media/areas/intro/intro.webp');

INSERT INTO rb_round (id, title, content, game_id)
VALUES (1, '序幕', '序幕', 1);

INSERT INTO rb_puzzle (id, title, ptype, content, content_type, judge, unlock_cond, round_id)
VALUES (1, '序幕', 1, '测试测试测试', 0,
        '[{"type": "exact","text":"a milestone","action":"milestone","result":"这是一个里程碑！"},{"type": "exact","text":"real answer","action":"correct","result":"回答正确！"}]',
        '{}', 1);

UPDATE rb_game SET intro_puzzle = 1
WHERE id = 1;

INSERT INTO rb_user (email, pass, urole, nickname)
VALUES ('test@rabbi.town', '$2b$12$tu7u2NM5PFaFcs3F.ZykLe8F2olKRQYH8zSQK9hybJdDZta8Pmnd6', 1, 'user_1');
