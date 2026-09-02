//! Tables transcribed directly from SMPTE ST 421:2013's own printed clauses
//! (fetched from `pub.smpte.org`, read cell-by-cell from the PDF's table
//! structure — not from memory, and not from any other decoder's source).
//!
//! Every table here carries the clause and table number it was read from so
//! a future re-check can go straight to the primary text. `cargo xtask
//! vlc-scan`'s tier-1 (prefix-free) check runs over every `VlcEntry` array
//! automatically; the runs/levels/deltas below were additionally spot-checked
//! by re-reading the source table image once more after transcription.

use vaco_codec_vlc::VlcEntry;

/// SS7.1.1.6 Table 36: `PQINDEX` (5 bits) to `(PQUANT, is_uniform)` when the
/// sequence's `QUANTIZER == 00b` (implicit quantizer). Index 0 is reserved
/// (`PQUANT` undefined) and never produced by a valid encoder.
pub(crate) const PQINDEX_TO_PQUANT: [(u8, bool); 32] = [
    (0, true), // 0: reserved, placeholder
    (1, true),
    (2, true),
    (3, true),
    (4, true),
    (5, true),
    (6, true),
    (7, true),
    (8, true),
    (6, false),
    (7, false),
    (8, false),
    (9, false),
    (10, false),
    (11, false),
    (12, false),
    (13, false),
    (14, false),
    (15, false),
    (16, false),
    (17, false),
    (18, false),
    (19, false),
    (20, false),
    (21, false),
    (22, false),
    (23, false),
    (24, false),
    (25, false),
    (27, false),
    (29, false),
    (31, false),
];

/// SS11.5 Table 168: I-Picture `CBPCY` VLC table. `symbol` is the six-bit
/// `decoded_cbpcy` value (SS8.1.2.1's `decoded_cbpcy`, before the
/// neighbour-prediction step that produces the real `cbpcy`).
pub(crate) const CBPCY_I: [VlcEntry; 64] = [
    VlcEntry::new(1, 1, 0),
    VlcEntry::new(23, 6, 1),
    VlcEntry::new(9, 5, 2),
    VlcEntry::new(5, 5, 3),
    VlcEntry::new(6, 5, 4),
    VlcEntry::new(71, 9, 5),
    VlcEntry::new(32, 7, 6),
    VlcEntry::new(16, 7, 7),
    VlcEntry::new(2, 5, 8),
    VlcEntry::new(124, 9, 9),
    VlcEntry::new(58, 7, 10),
    VlcEntry::new(29, 7, 11),
    VlcEntry::new(2, 6, 12),
    VlcEntry::new(236, 9, 13),
    VlcEntry::new(119, 8, 14),
    VlcEntry::new(0, 8, 15),
    VlcEntry::new(3, 5, 16),
    VlcEntry::new(183, 9, 17),
    VlcEntry::new(44, 7, 18),
    VlcEntry::new(19, 7, 19),
    VlcEntry::new(1, 6, 20),
    VlcEntry::new(360, 10, 21),
    VlcEntry::new(70, 8, 22),
    VlcEntry::new(63, 8, 23),
    VlcEntry::new(30, 6, 24),
    VlcEntry::new(1810, 13, 25),
    VlcEntry::new(181, 9, 26),
    VlcEntry::new(66, 8, 27),
    VlcEntry::new(34, 7, 28),
    VlcEntry::new(453, 11, 29),
    VlcEntry::new(286, 10, 30),
    VlcEntry::new(135, 9, 31),
    VlcEntry::new(6, 4, 32),
    VlcEntry::new(3, 9, 33),
    VlcEntry::new(30, 7, 34),
    VlcEntry::new(28, 6, 35),
    VlcEntry::new(18, 7, 36),
    VlcEntry::new(904, 12, 37),
    VlcEntry::new(68, 9, 38),
    VlcEntry::new(112, 9, 39),
    VlcEntry::new(31, 6, 40),
    VlcEntry::new(574, 11, 41),
    VlcEntry::new(57, 8, 42),
    VlcEntry::new(142, 9, 43),
    VlcEntry::new(1, 7, 44),
    VlcEntry::new(454, 11, 45),
    VlcEntry::new(182, 9, 46),
    VlcEntry::new(69, 9, 47),
    VlcEntry::new(20, 6, 48),
    VlcEntry::new(575, 11, 49),
    VlcEntry::new(125, 9, 50),
    VlcEntry::new(24, 9, 51),
    VlcEntry::new(7, 7, 52),
    VlcEntry::new(455, 11, 53),
    VlcEntry::new(134, 9, 54),
    VlcEntry::new(25, 9, 55),
    VlcEntry::new(21, 6, 56),
    VlcEntry::new(475, 10, 57),
    VlcEntry::new(2, 9, 58),
    VlcEntry::new(70, 9, 59),
    VlcEntry::new(13, 8, 60),
    VlcEntry::new(1811, 13, 61),
    VlcEntry::new(474, 10, 62),
    VlcEntry::new(361, 10, 63),
];

/// SS11.7.1 Table 173: Low-motion luma DC differential VLC table.
/// `symbol` 0..=118 is the DC differential magnitude; `symbol == 119` is
/// the escape sentinel ([`ESCAPE_DC`]).
pub(crate) const ESCAPE_DC: u32 = 119;

macro_rules! dc_table {
    ($name:ident, [$(($mag:literal, $code:literal, $len:literal)),+ $(,)?], $esc_code:literal, $esc_len:literal) => {
        pub(crate) const $name: [VlcEntry; count!($($mag),+) + 1] = [
            $(VlcEntry::new($code, $len, $mag),)+
            VlcEntry::new($esc_code, $esc_len, ESCAPE_DC),
        ];
    };
}
macro_rules! count {
    ($($x:literal),*) => { <[()]>::len(&[$(count!(@sub $x)),*]) };
    (@sub $x:literal) => { () };
}

dc_table!(
    DC_LOW_LUMA,
    [
        (0, 1, 1),
        (1, 1, 2),
        (2, 1, 4),
        (3, 1, 5),
        (4, 5, 5),
        (5, 7, 5),
        (6, 8, 6),
        (7, 12, 6),
        (8, 0, 7),
        (9, 2, 7),
        (10, 18, 7),
        (11, 26, 7),
        (12, 3, 8),
        (13, 7, 8),
        (14, 39, 8),
        (15, 55, 8),
        (16, 5, 9),
        (17, 76, 9),
        (18, 108, 9),
        (19, 109, 9),
        (20, 8, 10),
        (21, 25, 10),
        (22, 155, 10),
        (23, 27, 10),
        (24, 154, 10),
        (25, 19, 11),
        (26, 52, 11),
        (27, 53, 11),
        (28, 97, 12),
        (29, 72, 13),
        (30, 196, 13),
        (31, 74, 13),
        (32, 198, 13),
        (33, 199, 13),
        (34, 146, 14),
        (35, 395, 14),
        (36, 147, 14),
        (37, 387, 14),
        (38, 386, 14),
        (39, 150, 14),
        (40, 151, 14),
        (41, 384, 14),
        (42, 788, 15),
        (43, 789, 15),
        (44, 1541, 16),
        (45, 1540, 16),
        (46, 1542, 16),
        (47, 3086, 17),
        (48, 197_581, 23),
        (49, 197_577, 23),
        (50, 197_576, 23),
        (51, 197_578, 23),
        (52, 197_579, 23),
        (53, 197_580, 23),
        (54, 197_582, 23),
        (55, 197_583, 23),
        (56, 197_584, 23),
        (57, 197_585, 23),
        (58, 197_586, 23),
        (59, 197_587, 23),
        (60, 197_588, 23),
        (61, 197_589, 23),
        (62, 197_590, 23),
        (63, 197_591, 23),
        (64, 197_592, 23),
        (65, 197_593, 23),
        (66, 197_594, 23),
        (67, 197_595, 23),
        (68, 197_596, 23),
        (69, 197_597, 23),
        (70, 197_598, 23),
        (71, 197_599, 23),
        (72, 197_600, 23),
        (73, 197_601, 23),
        (74, 197_602, 23),
        (75, 197_603, 23),
        (76, 197_604, 23),
        (77, 197_605, 23),
        (78, 197_606, 23),
        (79, 197_607, 23),
        (80, 197_608, 23),
        (81, 197_609, 23),
        (82, 197_610, 23),
        (83, 197_611, 23),
        (84, 197_612, 23),
        (85, 197_613, 23),
        (86, 197_614, 23),
        (87, 197_615, 23),
        (88, 197_616, 23),
        (89, 197_617, 23),
        (90, 197_618, 23),
        (91, 197_619, 23),
        (92, 197_620, 23),
        (93, 197_621, 23),
        (94, 197_622, 23),
        (95, 197_623, 23),
        (96, 197_624, 23),
        (97, 197_625, 23),
        (98, 197_626, 23),
        (99, 197_627, 23),
        (100, 197_628, 23),
        (101, 197_629, 23),
        (102, 197_630, 23),
        (103, 197_631, 23),
        (104, 395_136, 24),
        (105, 395_137, 24),
        (106, 395_138, 24),
        (107, 395_139, 24),
        (108, 395_140, 24),
        (109, 395_141, 24),
        (110, 395_142, 24),
        (111, 395_143, 24),
        (112, 395_144, 24),
        (113, 395_145, 24),
        (114, 395_146, 24),
        (115, 395_147, 24),
        (116, 395_148, 24),
        (117, 395_149, 24),
        (118, 395_150, 24),
    ],
    395_151,
    24
);

dc_table!(
    DC_LOW_CHROMA,
    [
        (0, 0, 2),
        (1, 1, 2),
        (2, 5, 3),
        (3, 9, 4),
        (4, 13, 4),
        (5, 17, 5),
        (6, 29, 5),
        (7, 31, 5),
        (8, 33, 6),
        (9, 49, 6),
        (10, 56, 6),
        (11, 51, 6),
        (12, 57, 6),
        (13, 61, 6),
        (14, 97, 7),
        (15, 121, 7),
        (16, 128, 8),
        (17, 200, 8),
        (18, 202, 8),
        (19, 240, 8),
        (20, 129, 8),
        (21, 192, 8),
        (22, 201, 8),
        (23, 263, 9),
        (24, 262, 9),
        (25, 406, 9),
        (26, 387, 9),
        (27, 483, 9),
        (28, 482, 9),
        (29, 522, 10),
        (30, 523, 10),
        (31, 1545, 11),
        (32, 1042, 11),
        (33, 1043, 11),
        (34, 1547, 11),
        (35, 1041, 11),
        (36, 1546, 11),
        (37, 1631, 11),
        (38, 1040, 11),
        (39, 1629, 11),
        (40, 1630, 11),
        (41, 3256, 12),
        (42, 3088, 12),
        (43, 3257, 12),
        (44, 6179, 13),
        (45, 12_357, 14),
        (46, 24_713, 15),
        (47, 49_424, 16),
        (48, 3_163_208, 22),
        (49, 3_163_209, 22),
        (50, 3_163_210, 22),
        (51, 3_163_211, 22),
        (52, 3_163_212, 22),
        (53, 3_163_213, 22),
        (54, 3_163_214, 22),
        (55, 3_163_215, 22),
        (56, 3_163_216, 22),
        (57, 3_163_217, 22),
        (58, 3_163_218, 22),
        (59, 3_163_219, 22),
        (60, 3_163_220, 22),
        (61, 3_163_221, 22),
        (62, 3_163_222, 22),
        (63, 3_163_223, 22),
        (64, 3_163_224, 22),
        (65, 3_163_225, 22),
        (66, 3_163_226, 22),
        (67, 3_163_227, 22),
        (68, 3_163_228, 22),
        (69, 3_163_229, 22),
        (70, 3_163_230, 22),
        (71, 3_163_231, 22),
        (72, 3_163_232, 22),
        (73, 3_163_233, 22),
        (74, 3_163_234, 22),
        (75, 3_163_235, 22),
        (76, 3_163_236, 22),
        (77, 3_163_237, 22),
        (78, 3_163_238, 22),
        (79, 3_163_239, 22),
        (80, 3_163_240, 22),
        (81, 3_163_241, 22),
        (82, 3_163_242, 22),
        (83, 3_163_243, 22),
        (84, 3_163_244, 22),
        (85, 3_163_245, 22),
        (86, 3_163_246, 22),
        (87, 3_163_247, 22),
        (88, 3_163_248, 22),
        (89, 3_163_249, 22),
        (90, 3_163_250, 22),
        (91, 3_163_251, 22),
        (92, 3_163_252, 22),
        (93, 3_163_253, 22),
        (94, 3_163_254, 22),
        (95, 3_163_255, 22),
        (96, 3_163_256, 22),
        (97, 3_163_257, 22),
        (98, 3_163_258, 22),
        (99, 3_163_259, 22),
        (100, 3_163_260, 22),
        (101, 3_163_261, 22),
        (102, 3_163_262, 22),
        (103, 3_163_263, 22),
        (104, 6_326_400, 23),
        (105, 6_326_401, 23),
        (106, 6_326_402, 23),
        (107, 6_326_403, 23),
        (108, 6_326_404, 23),
        (109, 6_326_405, 23),
        (110, 6_326_406, 23),
        (111, 6_326_407, 23),
        (112, 6_326_408, 23),
        (113, 6_326_409, 23),
        (114, 6_326_410, 23),
        (115, 6_326_411, 23),
        (116, 6_326_412, 23),
        (117, 6_326_413, 23),
        (118, 6_326_414, 23),
    ],
    6_326_415,
    23
);

dc_table!(
    DC_HIGH_LUMA,
    [
        (0, 2, 2),
        (1, 3, 2),
        (2, 3, 3),
        (3, 2, 4),
        (4, 5, 4),
        (5, 1, 5),
        (6, 3, 5),
        (7, 8, 5),
        (8, 0, 6),
        (9, 5, 6),
        (10, 13, 6),
        (11, 15, 6),
        (12, 19, 6),
        (13, 8, 7),
        (14, 24, 7),
        (15, 28, 7),
        (16, 36, 7),
        (17, 4, 8),
        (18, 6, 8),
        (19, 18, 8),
        (20, 50, 8),
        (21, 59, 8),
        (22, 74, 8),
        (23, 75, 8),
        (24, 11, 9),
        (25, 38, 9),
        (26, 39, 9),
        (27, 102, 9),
        (28, 116, 9),
        (29, 117, 9),
        (30, 20, 10),
        (31, 28, 10),
        (32, 31, 10),
        (33, 29, 10),
        (34, 43, 11),
        (35, 61, 11),
        (36, 413, 11),
        (37, 415, 11),
        (38, 84, 12),
        (39, 825, 12),
        (40, 824, 12),
        (41, 829, 12),
        (42, 171, 13),
        (43, 241, 13),
        (44, 1656, 13),
        (45, 242, 13),
        (46, 480, 14),
        (47, 481, 14),
        (48, 340, 14),
        (49, 3314, 14),
        (50, 972, 15),
        (51, 683, 15),
        (52, 6631, 15),
        (53, 974, 15),
        (54, 6630, 15),
        (55, 1364, 16),
        (56, 1951, 16),
        (57, 1365, 16),
        (58, 3901, 17),
        (59, 3895, 17),
        (60, 3900, 17),
        (61, 3893, 17),
        (62, 7789, 18),
        (63, 7784, 18),
        (64, 15_576, 19),
        (65, 15_571, 19),
        (66, 15_577, 19),
        (67, 31_140, 20),
        (68, 996_538, 25),
        (69, 996_532, 25),
        (70, 996_533, 25),
        (71, 996_534, 25),
        (72, 996_535, 25),
        (73, 996_536, 25),
        (74, 996_537, 25),
        (75, 996_539, 25),
        (76, 996_540, 25),
        (77, 996_541, 25),
        (78, 996_542, 25),
        (79, 996_543, 25),
        (80, 1_993_024, 26),
        (81, 1_993_025, 26),
        (82, 1_993_026, 26),
        (83, 1_993_027, 26),
        (84, 1_993_028, 26),
        (85, 1_993_029, 26),
        (86, 1_993_030, 26),
        (87, 1_993_031, 26),
        (88, 1_993_032, 26),
        (89, 1_993_033, 26),
        (90, 1_993_034, 26),
        (91, 1_993_035, 26),
        (92, 1_993_036, 26),
        (93, 1_993_037, 26),
        (94, 1_993_038, 26),
        (95, 1_993_039, 26),
        (96, 1_993_040, 26),
        (97, 1_993_041, 26),
        (98, 1_993_042, 26),
        (99, 1_993_043, 26),
        (100, 1_993_044, 26),
        (101, 1_993_045, 26),
        (102, 1_993_046, 26),
        (103, 1_993_047, 26),
        (104, 1_993_048, 26),
        (105, 1_993_049, 26),
        (106, 1_993_050, 26),
        (107, 1_993_051, 26),
        (108, 1_993_052, 26),
        (109, 1_993_053, 26),
        (110, 1_993_054, 26),
        (111, 1_993_055, 26),
        (112, 1_993_056, 26),
        (113, 1_993_057, 26),
        (114, 1_993_058, 26),
        (115, 1_993_059, 26),
        (116, 1_993_060, 26),
        (117, 1_993_061, 26),
        (118, 1_993_062, 26),
    ],
    1_993_063,
    26
);

dc_table!(
    DC_HIGH_CHROMA,
    [
        (0, 0, 2),
        (1, 1, 2),
        (2, 4, 3),
        (3, 7, 3),
        (4, 11, 4),
        (5, 13, 4),
        (6, 21, 5),
        (7, 40, 6),
        (8, 48, 6),
        (9, 50, 6),
        (10, 82, 7),
        (11, 98, 7),
        (12, 102, 7),
        (13, 166, 8),
        (14, 198, 8),
        (15, 207, 8),
        (16, 335, 9),
        (17, 398, 9),
        (18, 412, 9),
        (19, 669, 10),
        (20, 826, 10),
        (21, 1336, 11),
        (22, 1596, 11),
        (23, 1598, 11),
        (24, 1599, 11),
        (25, 1654, 11),
        (26, 2675, 12),
        (27, 3194, 12),
        (28, 3311, 12),
        (29, 5349, 13),
        (30, 6621, 13),
        (31, 10_696, 14),
        (32, 10_697, 14),
        (33, 25_565, 15),
        (34, 13_240, 14),
        (35, 13_241, 14),
        (36, 51_126, 16),
        (37, 25_560, 15),
        (38, 25_567, 15),
        (39, 51_123, 16),
        (40, 51_124, 16),
        (41, 51_125, 16),
        (42, 25_566, 15),
        (43, 51_127, 16),
        (44, 51_128, 16),
        (45, 51_129, 16),
        (46, 102_245, 17),
        (47, 204_488, 18),
        (48, 13_087_304, 24),
        (49, 13_087_305, 24),
        (50, 13_087_306, 24),
        (51, 13_087_307, 24),
        (52, 13_087_308, 24),
        (53, 13_087_309, 24),
        (54, 13_087_310, 24),
        (55, 13_087_311, 24),
        (56, 13_087_312, 24),
        (57, 13_087_313, 24),
        (58, 13_087_314, 24),
        (59, 13_087_315, 24),
        (60, 13_087_316, 24),
        (61, 13_087_317, 24),
        (62, 13_087_318, 24),
        (63, 13_087_319, 24),
        (64, 13_087_320, 24),
        (65, 13_087_321, 24),
        (66, 13_087_322, 24),
        (67, 13_087_323, 24),
        (68, 13_087_324, 24),
        (69, 13_087_325, 24),
        (70, 13_087_326, 24),
        (71, 13_087_327, 24),
        (72, 13_087_328, 24),
        (73, 13_087_329, 24),
        (74, 13_087_330, 24),
        (75, 13_087_331, 24),
        (76, 13_087_332, 24),
        (77, 13_087_333, 24),
        (78, 13_087_334, 24),
        (79, 13_087_335, 24),
        (80, 13_087_336, 24),
        (81, 13_087_337, 24),
        (82, 13_087_338, 24),
        (83, 13_087_339, 24),
        (84, 13_087_340, 24),
        (85, 13_087_341, 24),
        (86, 13_087_342, 24),
        (87, 13_087_343, 24),
        (88, 13_087_344, 24),
        (89, 13_087_345, 24),
        (90, 13_087_346, 24),
        (91, 13_087_347, 24),
        (92, 13_087_348, 24),
        (93, 13_087_349, 24),
        (94, 13_087_350, 24),
        (95, 13_087_351, 24),
        (96, 13_087_352, 24),
        (97, 13_087_353, 24),
        (98, 13_087_354, 24),
        (99, 13_087_355, 24),
        (100, 13_087_356, 24),
        (101, 13_087_357, 24),
        (102, 13_087_358, 24),
        (103, 13_087_359, 24),
        (104, 26_174_592, 25),
        (105, 26_174_593, 25),
        (106, 26_174_594, 25),
        (107, 26_174_595, 25),
        (108, 26_174_596, 25),
        (109, 26_174_597, 25),
        (110, 26_174_598, 25),
        (111, 26_174_599, 25),
        (112, 26_174_600, 25),
        (113, 26_174_601, 25),
        (114, 26_174_602, 25),
        (115, 26_174_603, 25),
        (116, 26_174_604, 25),
        (117, 26_174_605, 25),
        (118, 26_174_606, 25),
    ],
    26_174_607,
    25
);

/// An AC coefficient coding set: SS8.1.3.4's `CodeTable` plus `RunTable`,
/// `LevelTable`, `StartIndexOfLast`, `EscapeIndex`, and the four escape-mode
/// delta tables. `run_level[i]` is `(run, level)` for VLC index `i`;
/// `i >= start_index_of_last` means `last_flag == 1` for that index.
pub(crate) struct AcCodingSet {
    pub(crate) code: &'static [VlcEntry],
    pub(crate) run_level: &'static [(u8, u8)],
    pub(crate) start_index_of_last: usize,
    pub(crate) escape_index: u32,
    /// Mode 1 (`NotLastDeltaLevelTable`), indexed by `run`.
    pub(crate) not_last_delta_level_by_run: &'static [u8],
    /// Mode 1 (`LastDeltaLevelTable`), indexed by `run`.
    pub(crate) last_delta_level_by_run: &'static [u8],
    /// Mode 2 (`NotLastDeltaRunTable`), indexed by `level - 1`.
    pub(crate) not_last_delta_run_by_level: &'static [u8],
    /// Mode 2 (`LastDeltaRunTable`), indexed by `level - 1`.
    pub(crate) last_delta_run_by_level: &'static [u8],
}

include!("tables_ac_data.rs");

/// SS11.8.6 Table 219-225: High Rate Intra coding set (Y blocks, coding-set
/// index 0, selected when `PQINDEX <= 8`).
pub(crate) const HIGH_RATE_INTRA: AcCodingSet = AcCodingSet {
    code: &HIGH_RATE_INTRA_CODE,
    run_level: &HIGH_RATE_INTRA_RUN_LEVEL,
    start_index_of_last: 126,
    escape_index: 162,
    not_last_delta_level_by_run: &HIGH_RATE_INTRA_NOT_LAST_DELTA_LEVEL_BY_RUN,
    last_delta_level_by_run: &HIGH_RATE_INTRA_LAST_DELTA_LEVEL_BY_RUN,
    not_last_delta_run_by_level: &HIGH_RATE_INTRA_NOT_LAST_DELTA_RUN_BY_LEVEL,
    last_delta_run_by_level: &HIGH_RATE_INTRA_LAST_DELTA_RUN_BY_LEVEL,
};

/// SS11.8.7 Table 226-232: High Rate Inter coding set (Cb/Cr blocks, coding-
/// set index 0, selected when `PQINDEX <= 8`).
pub(crate) const HIGH_RATE_INTER: AcCodingSet = AcCodingSet {
    code: &HIGH_RATE_INTER_CODE,
    run_level: &HIGH_RATE_INTER_RUN_LEVEL,
    start_index_of_last: 109,
    escape_index: 174,
    not_last_delta_level_by_run: &HIGH_RATE_INTER_NOT_LAST_DELTA_LEVEL_BY_RUN,
    last_delta_level_by_run: &HIGH_RATE_INTER_LAST_DELTA_LEVEL_BY_RUN,
    not_last_delta_run_by_level: &HIGH_RATE_INTER_NOT_LAST_DELTA_RUN_BY_LEVEL,
    last_delta_run_by_level: &HIGH_RATE_INTER_LAST_DELTA_RUN_BY_LEVEL,
};

/// SS7.1.4.7 Table 58: `ESCMODE` VLC (`1` -> Mode 1, `01` -> Mode 2,
/// `00` -> Mode 3).
pub(crate) const ESCMODE: [VlcEntry; 3] = [
    VlcEntry::new(0b1, 1, 1),
    VlcEntry::new(0b01, 2, 2),
    VlcEntry::new(0b00, 2, 3),
];

/// SS11.10 / SS7.1.4.10 Table 59: Escape Mode 3 level codeword size,
/// "conservative" table (`1 <= PQUANT <= 7`, or a `VOPDQUANT`-varying
/// picture — the latter never arises in this crate's constant-`MQUANT`
/// scope).
pub(crate) const ESCLVLSZ_CONSERVATIVE: [VlcEntry; 11] = [
    VlcEntry::new(0b001, 3, 1),
    VlcEntry::new(0b010, 3, 2),
    VlcEntry::new(0b011, 3, 3),
    VlcEntry::new(0b100, 3, 4),
    VlcEntry::new(0b101, 3, 5),
    VlcEntry::new(0b110, 3, 6),
    VlcEntry::new(0b111, 3, 7),
    VlcEntry::new(0b0_0000, 5, 8),
    VlcEntry::new(0b0_0001, 5, 9),
    VlcEntry::new(0b0_0010, 5, 10),
    VlcEntry::new(0b0_0011, 5, 11),
];

/// Table 60: Escape Mode 3 level codeword size, "efficient" table
/// (`8 <= PQUANT <= 31`).
pub(crate) const ESCLVLSZ_EFFICIENT: [VlcEntry; 7] = [
    VlcEntry::new(0b1, 1, 2),
    VlcEntry::new(0b01, 2, 3),
    VlcEntry::new(0b001, 3, 4),
    VlcEntry::new(0b0001, 4, 5),
    VlcEntry::new(0b0_0001, 5, 6),
    VlcEntry::new(0b00_0001, 6, 7),
    VlcEntry::new(0b00_0000, 6, 8),
];

/// SS7.1.4.11 Table 61: `ESCRUNSZ` (fixed 2 bits) to run codeword size.
pub(crate) const fn escrunsz_to_run_bits(escrunsz: u32) -> u32 {
    match escrunsz {
        0 => 3,
        1 => 4,
        2 => 5,
        _ => 6,
    }
}

/// SS11.9.1 Table 233: intra normal (zigzag) scan — used when `ACPRED == 0`.
/// `NORMAL_SCAN[i]` is the natural (row-major) position of the `i`-th
/// coefficient in decode order (Figure 43's `zigzagscan[i]`).
pub(crate) const NORMAL_SCAN: [usize; 64] = [
    0, 8, 1, 2, 9, 16, 24, 17, 10, 3, 4, 11, 18, 25, 32, 40, 33, 48, 26, 19, 12, 5, 6, 13, 20, 27,
    34, 41, 56, 49, 57, 42, 35, 28, 21, 14, 7, 15, 22, 29, 36, 43, 50, 58, 51, 59, 44, 37, 30, 23,
    31, 38, 45, 52, 60, 53, 61, 46, 39, 47, 54, 62, 55, 63,
];

/// Table 234: intra horizontal scan — `ACPRED == 1` with `prediction_direction == TOP`.
pub(crate) const HORIZONTAL_SCAN: [usize; 64] = [
    0, 1, 8, 2, 3, 9, 16, 24, 17, 10, 4, 5, 11, 18, 25, 32, 40, 48, 33, 26, 19, 12, 6, 7, 13, 20,
    27, 34, 41, 56, 49, 57, 42, 35, 28, 21, 14, 15, 22, 29, 36, 43, 50, 58, 51, 44, 37, 30, 23, 31,
    38, 45, 52, 59, 60, 53, 46, 39, 47, 54, 61, 62, 55, 63,
];

/// Table 235: intra vertical scan — `ACPRED == 1` with `prediction_direction == LEFT`.
pub(crate) const VERTICAL_SCAN: [usize; 64] = [
    0, 8, 16, 1, 24, 32, 40, 9, 2, 3, 10, 17, 25, 48, 56, 41, 33, 26, 18, 11, 4, 5, 12, 19, 27, 34,
    49, 57, 50, 42, 35, 28, 20, 13, 6, 7, 14, 21, 29, 36, 43, 51, 58, 59, 52, 44, 37, 30, 22, 15,
    23, 31, 38, 45, 60, 53, 46, 39, 47, 54, 61, 62, 55, 63,
];

#[cfg(test)]
mod tests {
    use super::*;
    use vaco_codec_vlc::is_prefix_free;

    fn is_permutation_of_0_63(scan: &[usize; 64]) -> bool {
        let mut seen = [false; 64];
        for &v in scan {
            let Some(slot) = seen.get_mut(v) else {
                return false;
            };
            if *slot {
                return false;
            }
            *slot = true;
        }
        seen.iter().all(|&s| s)
    }

    #[test]
    fn scans_are_permutations() {
        assert!(is_permutation_of_0_63(&NORMAL_SCAN));
        assert!(is_permutation_of_0_63(&HORIZONTAL_SCAN));
        assert!(is_permutation_of_0_63(&VERTICAL_SCAN));
    }

    #[test]
    fn cbpcy_is_prefix_free_and_covers_all_64_symbols() {
        assert!(is_prefix_free(&CBPCY_I));
        let mut seen = [false; 64];
        for e in &CBPCY_I {
            let Some(slot) = seen.get_mut(e.symbol as usize) else {
                unreachable!("symbol out of range")
            };
            *slot = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn dc_tables_are_prefix_free() {
        assert!(is_prefix_free(&DC_LOW_LUMA));
        assert!(is_prefix_free(&DC_LOW_CHROMA));
        assert!(is_prefix_free(&DC_HIGH_LUMA));
        assert!(is_prefix_free(&DC_HIGH_CHROMA));
    }

    #[test]
    fn ac_coding_sets_are_prefix_free_and_run_level_covers_escape_range() {
        for set in [&HIGH_RATE_INTRA, &HIGH_RATE_INTER] {
            assert!(is_prefix_free(set.code));
            assert_eq!(set.run_level.len(), set.escape_index as usize);
            assert!(set.start_index_of_last <= set.run_level.len());
        }
    }

    #[test]
    fn escmode_and_esclvlsz_are_prefix_free() {
        assert!(is_prefix_free(&ESCMODE));
        assert!(is_prefix_free(&ESCLVLSZ_CONSERVATIVE));
        assert!(is_prefix_free(&ESCLVLSZ_EFFICIENT));
    }

    #[test]
    fn pqindex_table_has_32_entries() {
        assert_eq!(PQINDEX_TO_PQUANT.len(), 32);
    }
}
