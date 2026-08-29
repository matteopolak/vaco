// Generated from SMPTE ST 421:2013 SS11.8.6/11.8.7 (Tables 219-232) by a
// script driven directly off the primary specification's own PDF text,
// table by table -- not retyped by hand, to remove the single biggest
// source of transcription error this class of table is known for in
// this project. Every table's parsed entry count was checked against the
// specification's own stated index range before this file was written.
// `tables.rs` includes this file and wires these into the two
// `AcCodingSet`s.

pub(crate) const HIGH_RATE_INTRA_CODE: [VlcEntry; 163] = [
    VlcEntry::new(0, 2, 0), VlcEntry::new(3, 3, 1), VlcEntry::new(13, 4, 2), VlcEntry::new(5, 4, 3), VlcEntry::new(28, 5, 4), VlcEntry::new(22, 5, 5),
    VlcEntry::new(63, 6, 6), VlcEntry::new(58, 6, 7), VlcEntry::new(46, 6, 8), VlcEntry::new(34, 6, 9), VlcEntry::new(123, 7, 10), VlcEntry::new(103, 7, 11),
    VlcEntry::new(95, 7, 12), VlcEntry::new(71, 7, 13), VlcEntry::new(38, 7, 14), VlcEntry::new(239, 8, 15), VlcEntry::new(205, 8, 16), VlcEntry::new(193, 8, 17),
    VlcEntry::new(169, 8, 18), VlcEntry::new(79, 8, 19), VlcEntry::new(498, 9, 20), VlcEntry::new(477, 9, 21), VlcEntry::new(409, 9, 22), VlcEntry::new(389, 9, 23),
    VlcEntry::new(349, 9, 24), VlcEntry::new(283, 9, 25), VlcEntry::new(1007, 10, 26), VlcEntry::new(993, 10, 27), VlcEntry::new(968, 10, 28), VlcEntry::new(817, 10, 29),
    VlcEntry::new(771, 10, 30), VlcEntry::new(753, 10, 31), VlcEntry::new(672, 10, 32), VlcEntry::new(563, 10, 33), VlcEntry::new(294, 10, 34), VlcEntry::new(1984, 11, 35),
    VlcEntry::new(1903, 11, 36), VlcEntry::new(1900, 11, 37), VlcEntry::new(1633, 11, 38), VlcEntry::new(1540, 11, 39), VlcEntry::new(1394, 11, 40), VlcEntry::new(1361, 11, 41),
    VlcEntry::new(1130, 11, 42), VlcEntry::new(628, 11, 43), VlcEntry::new(3879, 12, 44), VlcEntry::new(3876, 12, 45), VlcEntry::new(3803, 12, 46), VlcEntry::new(3214, 12, 47),
    VlcEntry::new(3083, 12, 48), VlcEntry::new(3082, 12, 49), VlcEntry::new(2787, 12, 50), VlcEntry::new(2262, 12, 51), VlcEntry::new(1168, 12, 52), VlcEntry::new(1173, 12, 53),
    VlcEntry::new(7961, 13, 54), VlcEntry::new(7605, 13, 55), VlcEntry::new(9, 4, 56), VlcEntry::new(16, 5, 57), VlcEntry::new(41, 6, 58), VlcEntry::new(98, 7, 59),
    VlcEntry::new(243, 8, 60), VlcEntry::new(173, 8, 61), VlcEntry::new(485, 9, 62), VlcEntry::new(377, 9, 63), VlcEntry::new(156, 9, 64), VlcEntry::new(945, 10, 65),
    VlcEntry::new(686, 10, 66), VlcEntry::new(295, 10, 67), VlcEntry::new(1902, 11, 68), VlcEntry::new(1392, 11, 69), VlcEntry::new(629, 11, 70), VlcEntry::new(3877, 12, 71),
    VlcEntry::new(3776, 12, 72), VlcEntry::new(2720, 12, 73), VlcEntry::new(2263, 12, 74), VlcEntry::new(7756, 13, 75), VlcEntry::new(8, 5, 76), VlcEntry::new(99, 7, 77),
    VlcEntry::new(175, 8, 78), VlcEntry::new(379, 9, 79), VlcEntry::new(947, 10, 80), VlcEntry::new(2013, 11, 81), VlcEntry::new(1600, 11, 82), VlcEntry::new(3981, 12, 83),
    VlcEntry::new(3009, 12, 84), VlcEntry::new(1169, 12, 85), VlcEntry::new(40, 6, 86), VlcEntry::new(195, 8, 87), VlcEntry::new(337, 9, 88), VlcEntry::new(673, 10, 89),
    VlcEntry::new(1395, 11, 90), VlcEntry::new(3779, 12, 91), VlcEntry::new(7989, 13, 92), VlcEntry::new(101, 7, 93), VlcEntry::new(474, 9, 94), VlcEntry::new(687, 10, 95),
    VlcEntry::new(631, 11, 96), VlcEntry::new(2249, 12, 97), VlcEntry::new(6017, 13, 98), VlcEntry::new(37, 7, 99), VlcEntry::new(280, 9, 100), VlcEntry::new(1606, 11, 101),
    VlcEntry::new(2726, 12, 102), VlcEntry::new(6016, 13, 103), VlcEntry::new(201, 8, 104), VlcEntry::new(801, 10, 105), VlcEntry::new(3995, 12, 106), VlcEntry::new(6430, 13, 107),
    VlcEntry::new(72, 8, 108), VlcEntry::new(1996, 11, 109), VlcEntry::new(2721, 12, 110), VlcEntry::new(384, 9, 111), VlcEntry::new(1125, 11, 112), VlcEntry::new(6405, 13, 113),
    VlcEntry::new(994, 10, 114), VlcEntry::new(3777, 12, 115), VlcEntry::new(15_515, 14, 116), VlcEntry::new(756, 10, 117), VlcEntry::new(2248, 12, 118), VlcEntry::new(1985, 11, 119),
    VlcEntry::new(2344, 13, 120), VlcEntry::new(1505, 11, 121), VlcEntry::new(12_813, 14, 122), VlcEntry::new(3778, 12, 123), VlcEntry::new(25_624, 15, 124), VlcEntry::new(7988, 13, 125),
    VlcEntry::new(120, 7, 126), VlcEntry::new(341, 9, 127), VlcEntry::new(1362, 11, 128), VlcEntry::new(6431, 13, 129), VlcEntry::new(250, 8, 130), VlcEntry::new(2012, 11, 131),
    VlcEntry::new(6407, 13, 132), VlcEntry::new(172, 8, 133), VlcEntry::new(585, 11, 134), VlcEntry::new(5041, 14, 135), VlcEntry::new(502, 9, 136), VlcEntry::new(2786, 12, 137),
    VlcEntry::new(476, 9, 138), VlcEntry::new(1261, 12, 139), VlcEntry::new(388, 9, 140), VlcEntry::new(6404, 13, 141), VlcEntry::new(342, 9, 142), VlcEntry::new(2521, 13, 143),
    VlcEntry::new(999, 10, 144), VlcEntry::new(2345, 13, 145), VlcEntry::new(946, 10, 146), VlcEntry::new(15_208, 14, 147), VlcEntry::new(757, 10, 148), VlcEntry::new(5040, 14, 149),
    VlcEntry::new(802, 10, 150), VlcEntry::new(15_209, 14, 151), VlcEntry::new(564, 10, 152), VlcEntry::new(31_029, 15, 153), VlcEntry::new(1991, 11, 154), VlcEntry::new(51_251, 16, 155),
    VlcEntry::new(1632, 11, 156), VlcEntry::new(31_028, 15, 157), VlcEntry::new(587, 11, 158), VlcEntry::new(51_250, 16, 159), VlcEntry::new(2727, 12, 160), VlcEntry::new(7960, 13, 161),
    VlcEntry::new(122, 7, 162),
];

pub(crate) const HIGH_RATE_INTRA_RUN_LEVEL: [(u8, u8); 162] = [
    (0, 1), (0, 2), (0, 3), (0, 4), (0, 5), (0, 6), (0, 7), (0, 8),
    (0, 9), (0, 10), (0, 11), (0, 12), (0, 13), (0, 14), (0, 15), (0, 16),
    (0, 17), (0, 18), (0, 19), (0, 20), (0, 21), (0, 22), (0, 23), (0, 24),
    (0, 25), (0, 26), (0, 27), (0, 28), (0, 29), (0, 30), (0, 31), (0, 32),
    (0, 33), (0, 34), (0, 35), (0, 36), (0, 37), (0, 38), (0, 39), (0, 40),
    (0, 41), (0, 42), (0, 43), (0, 44), (0, 45), (0, 46), (0, 47), (0, 48),
    (0, 49), (0, 50), (0, 51), (0, 52), (0, 53), (0, 54), (0, 55), (0, 56),
    (1, 1), (1, 2), (1, 3), (1, 4), (1, 5), (1, 6), (1, 7), (1, 8),
    (1, 9), (1, 10), (1, 11), (1, 12), (1, 13), (1, 14), (1, 15), (1, 16),
    (1, 17), (1, 18), (1, 19), (1, 20), (2, 1), (2, 2), (2, 3), (2, 4),
    (2, 5), (2, 6), (2, 7), (2, 8), (2, 9), (2, 10), (3, 1), (3, 2),
    (3, 3), (3, 4), (3, 5), (3, 6), (3, 7), (4, 1), (4, 2), (4, 3),
    (4, 4), (4, 5), (4, 6), (5, 1), (5, 2), (5, 3), (5, 4), (5, 5),
    (6, 1), (6, 2), (6, 3), (6, 4), (7, 1), (7, 2), (7, 3), (8, 1),
    (8, 2), (8, 3), (9, 1), (9, 2), (9, 3), (10, 1), (10, 2), (11, 1),
    (11, 2), (12, 1), (12, 2), (13, 1), (13, 2), (14, 1), (0, 1), (0, 2),
    (0, 3), (0, 4), (1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (2, 3),
    (3, 1), (3, 2), (4, 1), (4, 2), (5, 1), (5, 2), (6, 1), (6, 2),
    (7, 1), (7, 2), (8, 1), (8, 2), (9, 1), (9, 2), (10, 1), (10, 2),
    (11, 1), (11, 2), (12, 1), (12, 2), (13, 1), (13, 2), (14, 1), (14, 2),
    (15, 1), (16, 1),
];

pub(crate) const HIGH_RATE_INTRA_NOT_LAST_DELTA_LEVEL_BY_RUN: [u8; 15] = [
    56, 20, 10, 7, 6, 5, 4, 3, 3, 3, 2, 2,
    2, 2, 1,
];

pub(crate) const HIGH_RATE_INTRA_LAST_DELTA_LEVEL_BY_RUN: [u8; 17] = [
    4, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 1, 1,
];

pub(crate) const HIGH_RATE_INTRA_NOT_LAST_DELTA_RUN_BY_LEVEL: [u8; 56] = [
    14, 13, 9, 6, 5, 4, 3, 2, 2, 2, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
];

pub(crate) const HIGH_RATE_INTRA_LAST_DELTA_RUN_BY_LEVEL: [u8; 4] = [
    16, 14, 2, 0,
];

pub(crate) const HIGH_RATE_INTER_CODE: [VlcEntry; 175] = [
    VlcEntry::new(2, 2, 0), VlcEntry::new(0, 3, 1), VlcEntry::new(30, 5, 2), VlcEntry::new(4, 5, 3), VlcEntry::new(18, 6, 4), VlcEntry::new(112, 7, 5),
    VlcEntry::new(26, 7, 6), VlcEntry::new(95, 8, 7), VlcEntry::new(71, 8, 8), VlcEntry::new(467, 9, 9), VlcEntry::new(181, 9, 10), VlcEntry::new(87, 9, 11),
    VlcEntry::new(949, 10, 12), VlcEntry::new(365, 10, 13), VlcEntry::new(354, 10, 14), VlcEntry::new(1998, 11, 15), VlcEntry::new(1817, 11, 16), VlcEntry::new(1681, 11, 17),
    VlcEntry::new(710, 11, 18), VlcEntry::new(342, 11, 19), VlcEntry::new(3986, 12, 20), VlcEntry::new(3374, 12, 21), VlcEntry::new(3360, 12, 22), VlcEntry::new(1438, 12, 23),
    VlcEntry::new(1128, 12, 24), VlcEntry::new(678, 12, 25), VlcEntry::new(7586, 13, 26), VlcEntry::new(7264, 13, 27), VlcEntry::new(6723, 13, 28), VlcEntry::new(2845, 13, 29),
    VlcEntry::new(2240, 13, 30), VlcEntry::new(1373, 13, 31), VlcEntry::new(3, 3, 32), VlcEntry::new(10, 5, 33), VlcEntry::new(119, 7, 34), VlcEntry::new(229, 8, 35),
    VlcEntry::new(473, 9, 36), VlcEntry::new(997, 10, 37), VlcEntry::new(358, 10, 38), VlcEntry::new(1684, 11, 39), VlcEntry::new(338, 11, 40), VlcEntry::new(1439, 12, 41),
    VlcEntry::new(7996, 13, 42), VlcEntry::new(6731, 13, 43), VlcEntry::new(1374, 13, 44), VlcEntry::new(12, 4, 45), VlcEntry::new(125, 7, 46), VlcEntry::new(68, 8, 47),
    VlcEntry::new(992, 10, 48), VlcEntry::new(1897, 11, 49), VlcEntry::new(3633, 12, 50), VlcEntry::new(7974, 13, 51), VlcEntry::new(1372, 13, 52), VlcEntry::new(27, 5, 53),
    VlcEntry::new(226, 8, 54), VlcEntry::new(933, 10, 55), VlcEntry::new(713, 11, 56), VlcEntry::new(7971, 13, 57), VlcEntry::new(15_175, 14, 58), VlcEntry::new(7, 5, 59),
    VlcEntry::new(472, 9, 60), VlcEntry::new(728, 11, 61), VlcEntry::new(7975, 13, 62), VlcEntry::new(13_460, 14, 63), VlcEntry::new(53, 6, 64), VlcEntry::new(993, 10, 65),
    VlcEntry::new(1436, 12, 66), VlcEntry::new(14_531, 14, 67), VlcEntry::new(12, 6, 68), VlcEntry::new(357, 10, 69), VlcEntry::new(7459, 13, 70), VlcEntry::new(5688, 14, 71),
    VlcEntry::new(104, 7, 72), VlcEntry::new(1683, 11, 73), VlcEntry::new(14_917, 14, 74), VlcEntry::new(32, 7, 75), VlcEntry::new(3984, 12, 76), VlcEntry::new(31_990, 15, 77),
    VlcEntry::new(232, 8, 78), VlcEntry::new(1423, 12, 79), VlcEntry::new(11_503, 15, 80), VlcEntry::new(69, 8, 81), VlcEntry::new(2874, 13, 82), VlcEntry::new(497, 9, 83),
    VlcEntry::new(15_174, 14, 84), VlcEntry::new(423, 9, 85), VlcEntry::new(5750, 14, 86), VlcEntry::new(86, 9, 87), VlcEntry::new(26_922, 15, 88), VlcEntry::new(909, 10, 89),
    VlcEntry::new(58_121, 16, 90), VlcEntry::new(170, 10, 91), VlcEntry::new(116_241, 17, 92), VlcEntry::new(735, 11, 93), VlcEntry::new(46_009, 17, 94), VlcEntry::new(712, 11, 95),
    VlcEntry::new(232_480, 18, 96), VlcEntry::new(432, 11, 97), VlcEntry::new(91_024, 18, 98), VlcEntry::new(3999, 12, 99), VlcEntry::new(92_017, 18, 100), VlcEntry::new(3792, 12, 101),
    VlcEntry::new(464_963, 19, 102), VlcEntry::new(3370, 12, 103), VlcEntry::new(1_023_628, 20, 104), VlcEntry::new(1121, 12, 105), VlcEntry::new(1_023_630, 20, 106), VlcEntry::new(2919, 13, 107),
    VlcEntry::new(1375, 13, 108), VlcEntry::new(63, 6, 109), VlcEntry::new(109, 9, 110), VlcEntry::new(3728, 12, 111), VlcEntry::new(1358, 13, 112), VlcEntry::new(19, 6, 113),
    VlcEntry::new(281, 10, 114), VlcEntry::new(2918, 13, 115), VlcEntry::new(11, 6, 116), VlcEntry::new(565, 11, 117), VlcEntry::new(31_989, 15, 118), VlcEntry::new(117, 7, 119),
    VlcEntry::new(3364, 12, 120), VlcEntry::new(63_977, 16, 121), VlcEntry::new(46, 7, 122), VlcEntry::new(7970, 13, 123), VlcEntry::new(33, 7, 124), VlcEntry::new(1359, 13, 125),
    VlcEntry::new(20, 7, 126), VlcEntry::new(14_916, 14, 127), VlcEntry::new(228, 8, 128), VlcEntry::new(31_991, 15, 129), VlcEntry::new(94, 8, 130), VlcEntry::new(29_061, 15, 131),
    VlcEntry::new(55, 8, 132), VlcEntry::new(11_379, 15, 133), VlcEntry::new(475, 9, 134), VlcEntry::new(23_005, 16, 135), VlcEntry::new(455, 9, 136), VlcEntry::new(26_923, 15, 137),
    VlcEntry::new(422, 9, 138), VlcEntry::new(22_757, 16, 139), VlcEntry::new(180, 9, 140), VlcEntry::new(127_952, 17, 141), VlcEntry::new(176, 9, 142), VlcEntry::new(45_513, 17, 143),
    VlcEntry::new(998, 10, 144), VlcEntry::new(92_016, 18, 145), VlcEntry::new(366, 10, 146), VlcEntry::new(255_906, 18, 147), VlcEntry::new(283, 10, 148), VlcEntry::new(1_023_629, 20, 149),
    VlcEntry::new(217, 10, 150), VlcEntry::new(1_023_631, 20, 151), VlcEntry::new(168, 10, 152), VlcEntry::new(182_051, 19, 153), VlcEntry::new(1865, 11, 154), VlcEntry::new(929_924, 20, 155),
    VlcEntry::new(1686, 11, 156), VlcEntry::new(364_101, 20, 157), VlcEntry::new(734, 11, 158), VlcEntry::new(728_200, 21, 159), VlcEntry::new(561, 11, 160), VlcEntry::new(1_859_850, 21, 161),
    VlcEntry::new(433, 11, 162), VlcEntry::new(7_439_405, 23, 163), VlcEntry::new(3371, 12, 164), VlcEntry::new(3_719_703, 22, 165), VlcEntry::new(3375, 12, 166), VlcEntry::new(1_456_403, 22, 167),
    VlcEntry::new(1458, 12, 168), VlcEntry::new(1_456_402, 22, 169), VlcEntry::new(1129, 12, 170), VlcEntry::new(7_439_404, 23, 171), VlcEntry::new(6722, 13, 172), VlcEntry::new(2241, 13, 173),
    VlcEntry::new(115, 7, 174),
];

pub(crate) const HIGH_RATE_INTER_RUN_LEVEL: [(u8, u8); 174] = [
    (0, 1), (0, 2), (0, 3), (0, 4), (0, 5), (0, 6), (0, 7), (0, 8),
    (0, 9), (0, 10), (0, 11), (0, 12), (0, 13), (0, 14), (0, 15), (0, 16),
    (0, 17), (0, 18), (0, 19), (0, 20), (0, 21), (0, 22), (0, 23), (0, 24),
    (0, 25), (0, 26), (0, 27), (0, 28), (0, 29), (0, 30), (0, 31), (0, 32),
    (1, 1), (1, 2), (1, 3), (1, 4), (1, 5), (1, 6), (1, 7), (1, 8),
    (1, 9), (1, 10), (1, 11), (1, 12), (1, 13), (2, 1), (2, 2), (2, 3),
    (2, 4), (2, 5), (2, 6), (2, 7), (2, 8), (3, 1), (3, 2), (3, 3),
    (3, 4), (3, 5), (3, 6), (4, 1), (4, 2), (4, 3), (4, 4), (4, 5),
    (5, 1), (5, 2), (5, 3), (5, 4), (6, 1), (6, 2), (6, 3), (6, 4),
    (7, 1), (7, 2), (7, 3), (8, 1), (8, 2), (8, 3), (9, 1), (9, 2),
    (9, 3), (10, 1), (10, 2), (11, 1), (11, 2), (12, 1), (12, 2), (13, 1),
    (13, 2), (14, 1), (14, 2), (15, 1), (15, 2), (16, 1), (16, 2), (17, 1),
    (17, 2), (18, 1), (18, 2), (19, 1), (19, 2), (20, 1), (20, 2), (21, 1),
    (21, 2), (22, 1), (22, 2), (23, 1), (24, 1), (0, 1), (0, 2), (0, 3),
    (0, 4), (1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (2, 3), (3, 1),
    (3, 2), (3, 3), (4, 1), (4, 2), (5, 1), (5, 2), (6, 1), (6, 2),
    (7, 1), (7, 2), (8, 1), (8, 2), (9, 1), (9, 2), (10, 1), (10, 2),
    (11, 1), (11, 2), (12, 1), (12, 2), (13, 1), (13, 2), (14, 1), (14, 2),
    (15, 1), (15, 2), (16, 1), (16, 2), (17, 1), (17, 2), (18, 1), (18, 2),
    (19, 1), (19, 2), (20, 1), (20, 2), (21, 1), (21, 2), (22, 1), (22, 2),
    (23, 1), (23, 2), (24, 1), (24, 2), (25, 1), (25, 2), (26, 1), (26, 2),
    (27, 1), (27, 2), (28, 1), (28, 2), (29, 1), (30, 1),
];

pub(crate) const HIGH_RATE_INTER_NOT_LAST_DELTA_LEVEL_BY_RUN: [u8; 25] = [
    32, 13, 8, 6, 5, 4, 4, 3, 3, 3, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1,
    1,
];

pub(crate) const HIGH_RATE_INTER_LAST_DELTA_LEVEL_BY_RUN: [u8; 31] = [
    4, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 1, 1,
];

pub(crate) const HIGH_RATE_INTER_NOT_LAST_DELTA_RUN_BY_LEVEL: [u8; 32] = [
    24, 22, 9, 6, 4, 3, 2, 2, 1, 1, 1, 1,
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
];

pub(crate) const HIGH_RATE_INTER_LAST_DELTA_RUN_BY_LEVEL: [u8; 4] = [
    30, 28, 3, 0,
];

