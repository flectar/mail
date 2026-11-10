#![feature(test)]

/*
A benchmark for the possible strategies of looking up a flag in a flagset:

* `slice::contains` - roughly the same as `flagset.iter().any(|x| x.eq(flag))`.
* `slice::binary_search` - performs a binary search on the slice. Requires that the flagset be
  presorted.

Originally I thought that binary search in a sorted flagset would clearly be better but it's
actually typically slightly worse for common cases. When flagsets are small enough, binary search
adds more overhead than it's worth.

I took a histogram of the length of flagsets used in LibreOffice/dictionaries (see the
`flagset-histogram` branch):

```text
# of samples: 10352117
31.812710385711444'th percentile of data is 0 with 3293289 samples
68.81390540698101'th percentile of data is 1 with 3830407 samples
79.5990230790475'th percentile of data is 2 with 1116488 samples
86.29727619964109'th percentile of data is 3 with 693411 samples
90.34976130969153'th percentile of data is 4 with 419518 samples
93.03862195529669'th percentile of data is 5 with 278354 samples
95.01828466583213'th percentile of data is 6 with 204937 samples
95.84846268642443'th percentile of data is 7 with 85941 samples
96.32064629872325'th percentile of data is 8 with 48881 samples
96.89803544531036'th percentile of data is 9 with 59772 samples
97.34328736817794'th percentile of data is 10 with 46093 samples
97.78151657289035'th percentile of data is 11 with 45366 samples
98.05298761596299'th percentile of data is 12 with 28103 samples
98.37843795621707'th percentile of data is 13 with 33691 samples
98.85777952470977'th percentile of data is 14 with 49622 samples
99.00363374950264'th percentile of data is 15 with 15099 samples
99.37310407137014'th percentile of data is 16 with 38248 samples
99.53481012627658'th percentile of data is 17 with 16740 samples
99.67957278689953'th percentile of data is 18 with 14986 samples
99.71078379427126'th percentile of data is 19 with 3231 samples
99.73314636996471'th percentile of data is 20 with 2315 samples
99.79239995065744'th percentile of data is 21 with 6134 samples
99.81532279822571'th percentile of data is 22 with 2373 samples
99.833589593317'th percentile of data is 23 with 1891 samples
99.85790346071242'th percentile of data is 24 with 2517 samples
99.86442386615221'th percentile of data is 25 with 675 samples
99.9408623376262'th percentile of data is 26 with 7913 samples
99.94625253945642'th percentile of data is 27 with 558 samples
99.949546551686'th percentile of data is 28 with 341 samples
99.9525024688187'th percentile of data is 29 with 306 samples
99.95853022140302'th percentile of data is 30 with 624 samples
99.96063607086357'th percentile of data is 31 with 218 samples
99.96498300782342'th percentile of data is 33 with 450 samples
99.9685474961305'th percentile of data is 35 with 369 samples
99.97166763088168'th percentile of data is 37 with 323 samples
99.97396667754045'th percentile of data is 39 with 238 samples
99.97595660868207'th percentile of data is 41 with 206 samples
99.97818803632146'th percentile of data is 43 with 231 samples
99.97989783152566'th percentile of data is 45 with 177 samples
99.98148204855104'th percentile of data is 47 with 164 samples
99.98326912263454'th percentile of data is 49 with 185 samples
99.98466014246168'th percentile of data is 51 with 144 samples
99.98588694467036'th percentile of data is 53 with 127 samples
99.98694952926054'th percentile of data is 55 with 110 samples
99.98779959693267'th percentile of data is 57 with 88 samples
99.98877524278367'th percentile of data is 59 with 101 samples
99.9897895280743'th percentile of data is 61 with 105 samples
99.99072653448565'th percentile of data is 63 with 97 samples
99.99233007123084'th percentile of data is 67 with 166 samples
99.99370177133817'th percentile of data is 71 with 142 samples
99.994802995368'th percentile of data is 75 with 114 samples
99.9955178250014'th percentile of data is 79 with 74 samples
99.99609741659604'th percentile of data is 83 with 60 samples
99.9966866680506'th percentile of data is 87 with 61 samples
99.99709238216685'th percentile of data is 91 with 42 samples
99.99747877656328'th percentile of data is 95 with 40 samples
99.99779755194034'th percentile of data is 99 with 33 samples
99.9981163273174'th percentile of data is 103 with 33 samples
99.99845442241427'th percentile of data is 107 with 35 samples
99.99872489849177'th percentile of data is 111 with 28 samples
99.99893741540981'th percentile of data is 115 with 22 samples
99.99911129288822'th percentile of data is 119 with 18 samples
99.99918857176749'th percentile of data is 123 with 8 samples
99.99926585064678'th percentile of data is 127 with 8 samples
99.99943006826526'th percentile of data is 135 with 17 samples
99.99956530630402'th percentile of data is 143 with 14 samples
99.99966190490312'th percentile of data is 151 with 10 samples
99.99974884364232'th percentile of data is 159 with 9 samples
99.99983578238152'th percentile of data is 167 with 9 samples
99.99985510210135'th percentile of data is 175 with 2 samples
99.99988408168107'th percentile of data is 183 with 3 samples
99.99994204084054'th percentile of data is 191 with 6 samples
99.99996136056035'th percentile of data is 199 with 2 samples
99.99997102042026'th percentile of data is 207 with 1 samples
99.99998068028017'th percentile of data is 215 with 1 samples
99.99999034014009'th percentile of data is 231 with 1 samples
100'th percentile of data is 271 with 1 samples
```

Most words have exactly one flag. Any empty flagset is the second most popular. A quite vast
majority (90%) has four or fewer and we hit the 99th percentile with 15 flags in a flagset. This
breakdown also changes between dictionaries: en_US for example uses only small flagsets (around
ten at most).

Given that `contains` is faster than `binary_search` for up to the 99th percentile it might seem
worthwhile to switch to `contains`. `binary_search` though has much more predictable performance
when we hit these outliers that live in the low hundreds of flags.

```text
$ cargo bench
test binary_search_existing_flag_in_few_flags            ... bench:           1.11 ns/iter (+/- 0.06)
test binary_search_existing_flag_in_many_flags           ... bench:           9.49 ns/iter (+/- 0.20)
test binary_search_non_existing_flag_high_in_empty_flags ... bench:           0.69 ns/iter (+/- 0.00)
test binary_search_non_existing_flag_high_in_few_flags   ... bench:           1.11 ns/iter (+/- 0.00)
test binary_search_non_existing_flag_high_in_many_flags  ... bench:           9.48 ns/iter (+/- 0.30)
test binary_search_non_existing_flag_low_in_many_flags   ... bench:           9.50 ns/iter (+/- 0.19)
test contains_existing_flag_in_few_flags                 ... bench:           0.79 ns/iter (+/- 0.00)
test contains_existing_flag_in_many_flags                ... bench:          31.61 ns/iter (+/- 1.28)
test contains_non_existing_flag_high_in_empty_flags      ... bench:           0.46 ns/iter (+/- 0.00)
test contains_non_existing_flag_high_in_few_flags        ... bench:           0.78 ns/iter (+/- 0.00)
test contains_non_existing_flag_high_in_many_flags       ... bench:          45.23 ns/iter (+/- 1.17)
test contains_non_existing_flag_low_in_many_flags        ... bench:          45.02 ns/iter (+/- 4.91)
```

I think the tradeoff is worthwhile: we pay very slightly more on average but have no
degenerate cases.

*/

extern crate test;

use test::{black_box, Bencher};

type Flag = std::num::NonZeroU16;

const fn flag_n(n: u16) -> Flag {
    assert!(n != 0);

    unsafe { Flag::new_unchecked(n) }
}

const fn flag(ch: char) -> Flag {
    assert!(ch as u32 != 0);

    unsafe { Flag::new_unchecked(ch as u16) }
}

// be_BY.dic (Belarusian) `абвал/2,9,10,12,13,16,17,22,23,62,67,68,69,70,74,250,270,290,296,297,298,299,300,322,335,363,364,365,367,368,398,399,400,403,408,423,424,425,426,427,479,514,520,521,522,523,524,525,526,527,528,529,530,543,577,585,633,634,635,639,640,641,642,643,647,648,649,650,652,726,747,773,774,775,778,794,836,838,1076,1082,1087,1088,1089,1090,1091,1092,1093,1094,1095,1096,1097,1175,1276,1695,1696,1697,1704,1705,1706,1707,1708,1709,1710,1711,1902,1903,1904,1905,1906,1907,1908,1909,1910,1911,1912,1992,1993,2055,2056,2057,2058,2059,2060,2130,2429,2668,2875,2876,2877,2878,2879,3185,3186,3187,3188,3189,3190,3191,3192,3193,3194,3316,3317,3318,3600,3726,3949,4381,4382,4383,4384,4385,4386`
#[rustfmt::skip]
const MANY_FLAGS: [Flag; 153] = [flag_n(2), flag_n(9), flag_n(10), flag_n(12), flag_n(13), flag_n(16), flag_n(17), flag_n(22), flag_n(23), flag_n(62), flag_n(67), flag_n(68), flag_n(69), flag_n(70), flag_n(74), flag_n(250), flag_n(270), flag_n(290), flag_n(296), flag_n(297), flag_n(298), flag_n(299), flag_n(300), flag_n(322), flag_n(335), flag_n(363), flag_n(364), flag_n(365), flag_n(367), flag_n(368), flag_n(398), flag_n(399), flag_n(400), flag_n(403), flag_n(408), flag_n(423), flag_n(424), flag_n(425), flag_n(426), flag_n(427), flag_n(479), flag_n(514), flag_n(520), flag_n(521), flag_n(522), flag_n(523), flag_n(524), flag_n(525), flag_n(526), flag_n(527), flag_n(528), flag_n(529), flag_n(530), flag_n(543), flag_n(577), flag_n(585), flag_n(633), flag_n(634), flag_n(635), flag_n(639), flag_n(640), flag_n(641), flag_n(642), flag_n(643), flag_n(647), flag_n(648), flag_n(649), flag_n(650), flag_n(652), flag_n(726), flag_n(747), flag_n(773), flag_n(774), flag_n(775), flag_n(778), flag_n(794), flag_n(836), flag_n(838), flag_n(1076), flag_n(1082), flag_n(1087), flag_n(1088), flag_n(1089), flag_n(1090), flag_n(1091), flag_n(1092), flag_n(1093), flag_n(1094), flag_n(1095), flag_n(1096), flag_n(1097), flag_n(1175), flag_n(1276), flag_n(1695), flag_n(1696), flag_n(1697), flag_n(1704), flag_n(1705), flag_n(1706), flag_n(1707), flag_n(1708), flag_n(1709), flag_n(1710), flag_n(1711), flag_n(1902), flag_n(1903), flag_n(1904), flag_n(1905), flag_n(1906), flag_n(1907), flag_n(1908), flag_n(1909), flag_n(1910), flag_n(1911), flag_n(1912), flag_n(1992), flag_n(1993), flag_n(2055), flag_n(2056), flag_n(2057), flag_n(2058), flag_n(2059), flag_n(2060), flag_n(2130), flag_n(2429), flag_n(2668), flag_n(2875), flag_n(2876), flag_n(2877), flag_n(2878), flag_n(2879), flag_n(3185), flag_n(3186), flag_n(3187), flag_n(3188), flag_n(3189), flag_n(3190), flag_n(3191), flag_n(3192), flag_n(3193), flag_n(3194), flag_n(3316), flag_n(3317), flag_n(3318), flag_n(3600), flag_n(3726), flag_n(3949), flag_n(4381), flag_n(4382), flag_n(4383), flag_n(4384), flag_n(4385), flag_n(4386)];

// en_US.dic `advent/SM` (sorted)
const FEW_FLAGS: [Flag; 2] = [flag('M'), flag('S')];

const EMPTY_FLAGS: [Flag; 0] = [];

const UNKNOWN_FLAG_HIGH: Flag = flag_n(5000);
const UNKNOWN_FLAG_LOW: Flag = flag_n(1);

const FLAG_S: Flag = flag('S');
const FLAG_1709: Flag = flag_n(1709);

#[bench]
fn contains_non_existing_flag_high_in_many_flags(b: &mut Bencher) {
    b.iter(|| black_box(MANY_FLAGS).contains(black_box(&UNKNOWN_FLAG_HIGH)))
}

#[bench]
fn binary_search_non_existing_flag_high_in_many_flags(b: &mut Bencher) {
    b.iter(|| black_box(MANY_FLAGS).binary_search(black_box(&UNKNOWN_FLAG_HIGH)))
}

//---

#[bench]
fn contains_non_existing_flag_low_in_many_flags(b: &mut Bencher) {
    b.iter(|| black_box(MANY_FLAGS).contains(black_box(&UNKNOWN_FLAG_LOW)))
}

#[bench]
fn binary_search_non_existing_flag_low_in_many_flags(b: &mut Bencher) {
    b.iter(|| black_box(MANY_FLAGS).binary_search(black_box(&UNKNOWN_FLAG_LOW)))
}

//---

#[bench]
fn contains_existing_flag_in_many_flags(b: &mut Bencher) {
    b.iter(|| black_box(MANY_FLAGS).contains(black_box(&FLAG_1709)))
}

#[bench]
fn binary_search_existing_flag_in_many_flags(b: &mut Bencher) {
    b.iter(|| black_box(MANY_FLAGS).binary_search(black_box(&FLAG_1709)))
}

//---

#[bench]
fn contains_non_existing_flag_high_in_few_flags(b: &mut Bencher) {
    b.iter(|| black_box(FEW_FLAGS).contains(black_box(&UNKNOWN_FLAG_HIGH)))
}

#[bench]
fn binary_search_non_existing_flag_high_in_few_flags(b: &mut Bencher) {
    b.iter(|| black_box(FEW_FLAGS).binary_search(black_box(&UNKNOWN_FLAG_HIGH)))
}

//---

#[bench]
fn contains_existing_flag_in_few_flags(b: &mut Bencher) {
    b.iter(|| black_box(FEW_FLAGS).contains(black_box(&FLAG_S)))
}

#[bench]
fn binary_search_existing_flag_in_few_flags(b: &mut Bencher) {
    b.iter(|| black_box(FEW_FLAGS).binary_search(black_box(&FLAG_S)))
}

//---

#[bench]
fn contains_non_existing_flag_high_in_empty_flags(b: &mut Bencher) {
    b.iter(|| black_box(EMPTY_FLAGS).contains(black_box(&UNKNOWN_FLAG_HIGH)))
}

#[bench]
fn binary_search_non_existing_flag_high_in_empty_flags(b: &mut Bencher) {
    b.iter(|| black_box(EMPTY_FLAGS).binary_search(black_box(&UNKNOWN_FLAG_HIGH)))
}
