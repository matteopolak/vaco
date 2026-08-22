# 07 — Legal, Patent & Licensing Risk Register

**Project:** `vaco` — clean-room Rust reimplementation of `ffmpeg` / `ffprobe` / `ffplay`
**Target licence of deliverable:** MIT (see §4.2 — recommendation is to change this to `MIT OR Apache-2.0`)
**Goal:** the project distributes its own binaries.
**Reference tree:** `~/repos/FFmpeg` @ `564f92cce23ae95399476617b8a1dc357f002a47` (2026-08-18, `RELEASE` = `8.0.git`)
**Document date:** 2026-08-21
**Status:** engineering risk register — living document

---

## ⚠️ THIS IS NOT LEGAL ADVICE

I am not a lawyer and this is not legal advice. This document is an **engineering risk register**: it collects
publicly-available facts, cites them, and converts them into RED / AMBER / GREEN engineering decisions so the team
can build something and know where the landmines are. Every RED item, and the five questions in §5.4, require
qualified counsel in the relevant jurisdictions before the project distributes binaries commercially or at scale.

Patent status in particular is **jurisdiction-specific, changes continuously, and cannot be resolved by reading web
pages**. Expiry dates below are "last known essential patent in the pool, US" and are indicative only. Pool
membership is not exhaustive — unpooled holders exist for every major codec.

---

## 0. Executive summary

| # | Decision | Verdict |
|---|---|---|
| 1 | Is a Rust rewrite from a spec a derivative work of FFmpeg? | Not if done properly. Copyright attaches to **expression**, not function. Risk is evidentiary, not doctrinal — so the mitigation is **provenance evidence**, not cleverness. |
| 2 | Do we need a formal two-team dirty/clean split? | **No, not for everything.** Spec-first is sufficient for the ~90% of formats with a public spec. Reserve the two-team protocol for reverse-engineered formats with no spec (§1.7). |
| 3 | Does rewriting in Rust reduce patent exposure? | **No. Zero. None.** Patents cover methods, not source code. (§2.6) |
| 4 | Can we ship binaries? | Yes — for a **restricted default codec set**. Not for HEVC/VVC/AAC-family/DTS/Dolby-modern. (§5) |
| 5 | MIT-only or `MIT OR Apache-2.0`? | **`MIT OR Apache-2.0`.** Firm recommendation. (§4.2) |

**The single most important finding:** the reason FFmpeg does not ship binaries is *primarily* patents, and a
clean-room Rust rewrite does **nothing** about that. Our distributable-binary problem is a **codec selection
problem**, not a licensing or language problem.

---

## 1. Copyright and clean-room

### 1.1 What "clean room" actually means here

"Clean room" (or "Chinese wall") is an **evidentiary** technique, not a legal doctrine. Nothing in copyright law
says "clean rooms are legal". What the technique does is destroy the plaintiff's ability to prove the two elements
of a copyright infringement claim for non-literal copying:

1. **Access** to the protected work, and
2. **Substantial similarity** of protected expression.

If the person who wrote the code provably never saw the original, the plaintiff cannot establish access, and
similarity in *function* is then explained by the shared specification rather than by copying. The historical
reference case is Phoenix Technologies' 1984 re-implementation of the IBM PC BIOS, which enabled the Compaq
clone market — one team read the IBM BIOS listing and wrote a functional specification, a second team who had
never seen the listing implemented from that specification.

**What copyright does and does not protect in software:**

- Copyright protects **expression**, never **ideas, methods of operation, or functionality**
  (17 U.S.C. § 102(b); *Baker v. Selden*, 101 U.S. 99 (1879)).
- *Computer Associates v. Altai*, 982 F.2d 693 (2d Cir. 1992) established the **abstraction–filtration–comparison**
  test: break the program into levels of abstraction, **filter out** elements dictated by efficiency, by external
  factors (hardware, interoperability requirements, industry practice, **standards**), and elements taken from the
  public domain — then compare only what remains.
  <https://law.justia.com/cases/federal/appellate-courts/F2/982/693/145131/>
- *Lotus v. Borland*, 49 F.3d 807 (1st Cir. 1995), aff'd by an equally divided Supreme Court 516 U.S. 233 (1996):
  a menu command hierarchy is an uncopyrightable **"method of operation"** under § 102(b).
- *Sega v. Accolade*, 977 F.2d 1510 (9th Cir. 1992) and *Sony v. Connectix*, 203 F.3d 596 (9th Cir. 2000):
  intermediate copying (disassembly) to discover unprotected functional elements is **fair use** where it is the
  only way to get at those elements.
- *Feist v. Rural Telephone*, 499 U.S. 340 (1991): "sweat of the brow" is **not** a basis for copyright.
  Compilations of facts require **originality in selection, coordination, or arrangement**. Directly relevant to
  constant tables (§1.5b).

**EU position (materially more favourable to us than the US):**

- Directive 2009/24/EC Art. 1(2): "Ideas and principles which underlie any element of a computer program,
  including those which underlie its interfaces, are not protected by copyright."
  Art. 5(3) and Art. 6 additionally permit observation/testing and limited decompilation for interoperability, and
  **those rights cannot be contracted away** (Art. 8).
  <https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX%3A32009L0024>
- *SAS Institute v. World Programming*, CJEU C-406/10 (2 May 2012): **the functionality of a computer program, the
  programming language, and the format of data files are not a form of expression** and are therefore not protected
  by copyright as computer programs. This is close to a direct holding that reimplementing behaviour and file
  formats is lawful in the EU. <https://curia.europa.eu/juris/liste.jsf?num=C-406/10>

> **Uncertain / needs counsel:** the UK post-Brexit position broadly tracks *SAS* (the UK case went to the Court of
> Appeal and WPL won), but this should be confirmed if the UK is a target market.

### 1.2 Is reading LGPL source and then writing Rust a derivative work?

**Short answer: not automatically — but it is the single highest-risk thing an engineer on this project can do,
and our policy forbids it for implementers.**

The long answer has three parts:

1. **Doctrinally**, writing new code that performs the same function is not infringement. Under
   abstraction–filtration–comparison, everything dictated by the bitstream specification gets filtered out before
   comparison. Two conforming H.264 decoders necessarily share their entire functional skeleton; that shared
   skeleton is *scènes à faire* for this domain and is unprotectable.

2. **Practically**, a human who has just read `libavcodec/h264_cabac.c` and then writes Rust will reproduce
   FFmpeg's **non-obvious choices**: its variable names, its function decomposition, its loop fusion decisions, its
   specific optimisation tricks, its comment placement, its error-handling structure. Those choices are *not*
   dictated by the spec, they are FFmpeg's expression, and reproducing them is what makes the resulting file look
   like a translation. **Translation between programming languages is explicitly a derivative work** — 17 U.S.C.
   § 101 defines a derivative work to include a work "recast, transformed, or adapted". A line-by-line C→Rust port
   is the paradigm case of adaptation, and would be infringing.

3. **Evidentially**, once access is established, the burden shifts. If an engineer's browser history and git
   history show them reading FFmpeg source the same week they committed a suspiciously similar module, we have no
   defence that costs less than a trial.

The FSF's own position (which is not law, but is the licensor's stated interpretation, and FFmpeg's copyright
holders are the ones who would sue) is aggressive about what counts as derivative. We do not need to win that
argument; we need to never have it.

**Our rule:** *implementers do not read FFmpeg's C source.* Full stop. See §1.6.

### 1.3 The accepted two-team model

| | **Dirty team ("readers", "Team A")** | **Clean team ("implementers", "Team B")** |
|---|---|---|
| May read | FFmpeg source, other GPL/LGPL source, disassembly, leaked docs (no), anything lawfully obtained | Only the specification documents Team B is handed, plus public specs/RFCs |
| Produces | A **functional specification** in prose + pseudocode + tables, containing *no* source code, *no* verbatim comments, *no* identifiers copied from the original | The implementation |
| Never | Writes shipped code; talks to Team B outside the reviewed artefact channel | Reads the original; asks Team A "how did FFmpeg do it?" |
| Gatekeeper | A **reviewer** (ideally not on either team) inspects every spec document before it crosses the wall, and strips anything that reads like expression rather than fact | |

The wall is only as good as the **gatekeeper** and the **records**. An undocumented clean room is worth almost
nothing in litigation because you cannot prove it happened.

### 1.4 Safe vs unsafe — the operative table

**SAFE (green — do freely):**

| Activity | Why |
|---|---|
| Reading published specs: ITU-T H.26x, ISO/IEC 14496/23008/23090, IETF RFCs, SMPTE, AES, ETSI/3GPP | Specs describe the method, and are published for the purpose of enabling implementation. (Note: **copyright still subsists in the spec text** — see below.) |
| Reading academic papers on the algorithms | Public disclosure; ideas unprotected |
| **Black-box conformance testing** against the `ffmpeg` binary: feeding inputs, capturing outputs, diffing | Observing behaviour is fact-gathering; EU Directive Art. 5(3) expressly permits it |
| Reading FFmpeg's **documentation** (`doc/*.texi`, man pages) to learn CLI behaviour | Learning the interface is not copying the implementation. But **do not paste doc text** into our docs (§1.5a). |
| Running `ffmpeg -h full`, `ffprobe -show_streams`, etc., and recording observed behaviour | Same |
| Reading FFmpeg's **bug tracker, mailing list, commit messages** for behavioural facts | Facts about behaviour |
| Reading **permissively-licensed** reference implementations (BSD/MIT/Apache) — e.g. libopus, libvpx, dav1d, the JM/HM reference software where its licence permits | Different problem entirely: this is a **licence compliance** question, not a clean-room question. See §4. |
| Using ITU/ISO **conformance bitstreams** as test inputs | Using a test file as input is not copying it into our product |

**UNSAFE (red — forbidden by policy):**

| Activity | Why |
|---|---|
| Reading FFmpeg C and then writing the equivalent Rust | §1.2 |
| Any mechanical or AI-assisted C→Rust translation of FFmpeg code | Paradigm derivative work; also defeats every provenance record we keep |
| Copying constant tables **out of FFmpeg source** (even spec-dictated ones) | See §1.5b — the *table* may be unprotectable but **copying from FFmpeg** destroys our provenance story. Transcribe from the spec instead. |
| Copying FFmpeg's `AVOption` tables verbatim (the C initialiser structs) | The struct-initialiser text is expression, even if the option *names* are not (§1.5a) |
| Copying comments, `TODO`s, or the prose in FFmpeg headers | Pure expression, zero functional necessity |
| Copying FFmpeg's `configure` script logic or build-system text | Expression |
| Copying FATE reference checksums into our test suite **as our expected values** | §1.5d — see the nuance there |
| Copying FFmpeg's `ffprobe.xsd` file | The *file* is expression; the *field names* likely are not (§1.5e) |
| Pasting spec text into our source comments | Specs are copyrighted. Paraphrase, cite the clause number. |

### 1.5 The five grey areas

#### (a) CLI option names and semantics — **AMBER, leaning GREEN**

This is the *Google v. Oracle* question, and the honest answer is that the law is unsettled but the practical risk
is low.

- **Oracle America v. Google**, 750 F.3d 1339 (Fed. Cir. 2014): reversed the district court and held that the
  **structure, sequence and organisation of the Java API declaring code IS copyrightable**, rejecting the argument
  that it was a § 102(b) method of operation. This holding was never overruled.
- **Google LLC v. Oracle America**, 593 U.S. 1 (2021): the Supreme Court **assumed without deciding** that the
  declaring code was copyrightable, and held that Google's copying of ~11,500 lines of declaring code (plus the
  SSO of the packages) was **fair use**. Breyer, J., for a 6–2 Court, weighted: the declaring code sits far from
  the core of copyright ("inherently bound together with uncopyrightable ideas"); Google's use was
  transformative (a new smartphone platform); the amount copied was 0.4% of the API and was taken because
  programmers had invested in learning it; and the market harm analysis favoured Google.
  <https://www.supremecourt.gov/opinions/20pdf/18-956_d18f.pdf>

**How this maps to us.** `-i`, `-c:v`, `-b:v 2M`, `-vf scale=1280:-2`, `-preset slow`, `-movflags +faststart` are:

- **Much closer to *Lotus v. Borland*'s uncopyrightable menu command hierarchy than to the Java API.** A CLI flag
  set has no "declaring code", no SSO of nested packages, no header files. It is a command vocabulary — a method
  of operation for driving the program.
- Even if protectable, our use is squarely within the *Google* fair-use rationale: reimplementing an interface so
  that the enormous installed base of scripts and human knowledge continues to work is exactly the
  "programmers have invested in learning it" interest the Court credited.
- Individual short strings (`-crf`, `faststart`) are almost certainly below the threshold of originality anyway
  (words and short phrases are not copyrightable — 37 C.F.R. § 202.1(a)).

**Verdict: implement the CLI surface, including option names and semantics. GREEN with an asterisk.**

**But:** do *not* copy the option **help strings**, the **descriptions**, or the C `AVOption` table text. Those are
prose, and prose is expression. Write our own help text. Also do not copy the FFmpeg man pages / `.texi` docs.

> **Lawyer question #1 (§5.4):** confirm the CLI-surface reimplementation position for the target jurisdictions,
> and confirm that help-text rewriting is sufficient separation.

#### (b) Constant tables — **it depends, and the distinction is real**

Under *Feist*, a table is copyrightable only if there is **originality in selection, coordination, or arrangement**.
Apply *Altai* filtration on top of that. This gives three tiers:

**Tier 1 — dictated by a published specification → not protectable as against us (GREEN, transcribe from the spec):**
- H.264 CABAC context initialisation tables (ITU-T H.264 Tables 9-12 ff.)
- Zigzag / field scan orders, MPEG-1/2/4 default intra & inter quantiser matrices *as printed in the standard*
- CRC polynomials, Reed-Solomon generator tables
- Huffman/VLC code tables printed in a spec (MP3 Layer III tables in ISO/IEC 11172-3, AAC codebooks in 14496-3)
- Colour-primary / transfer-characteristic matrices (ITU-T H.273)
- AC-3 bit-allocation tables in ATSC A/52

These are the definition of **merger** (there is exactly one way to express "the table the spec mandates") and
*scènes à faire* (any conforming implementation must contain them). FFmpeg's copyright in
`libavcodec/aactab.c` cannot stop us having the same numbers, because those numbers are the standard.

**⚠ Critical process point:** *the legal conclusion does not change our workflow.* Transcribe tables **from the
specification document**, never from FFmpeg's `.c`/`.h` files, and record which spec clause each table came from.
Two reasons: (i) it proves provenance; (ii) FFmpeg's tables sometimes contain FFmpeg-specific **reordering,
pre-scaling, or packing** for their implementation's convenience — those transformations *are* FFmpeg's original
contribution and copying them is copying expression.

**Tier 2 — author's original choices → AMBER/RED, do not copy:**
- Hand-tuned quantiser matrices that are *not* the spec defaults (e.g. x264's `--tune` psy matrices, FFmpeg's
  custom presets)
- Psychoacoustic model constants that an implementer tuned by ear
- Encoder rate-control heuristic constants, lambda tables, mode-decision cost tables
- Hash seeds and magic constants chosen arbitrarily by an author
- FFmpeg's `ff_*` lookup tables that are *derived* (pre-computed, pre-scaled, interleaved) rather than transcribed

These reflect selection and judgement. Even where an individual number is a fact, the **compilation** may be
protected. Derive our own, or generate them at build time from first principles with our own code.

**Tier 3 — generated tables:** prefer generating tables at build time (a `build.rs` or `const fn`) from the
mathematical definition in the spec. This is both the cleanest provenance story and self-documenting. FFmpeg does
this itself (see `libavcodec/cbrt_tablegen.c`, `aacps_tablegen.c` in the reference tree) — the *idea* of generating
is free to copy; the generator code is not.

> **Lawyer question #2 (§5.4):** the Tier-1 position on spec-dictated tables. This is the one I would most want a
> written opinion on, because it touches every codec we implement.

#### (c) File-format magic numbers and structure — **GREEN**

- Magic numbers (`ftyp`, `RIFF`, `0x1A45DFA3`, `ID3`, `OggS`) are **short strings/facts**: not copyrightable
  (37 C.F.R. § 202.1(a)), and functionally mandated — pure merger.
- Container *structure* (box hierarchies, atom layouts, EBML element IDs) is a **data file format**, which
  *SAS Institute v. WPL* (C-406/10) holds is **not protected by copyright** in the EU, and which in the US is
  filtered out under *Altai* as dictated by external interoperability requirements.
- The *specification document* describing the format is copyrighted (ISO/IEC 14496-12 costs money and you may not
  redistribute it), but the format itself is not.

**Caveat:** some formats carry *patent* claims (the FFmpeg FAQ notes Microsoft's ASF patent claim — `doc/faq.texi`
line ~55) or *trade secret* / NDA encumbrance (DTS, Dolby container details, some broadcast formats). That is a
different regime — see §2.

#### (d) Test vectors and reference checksums — **AMBER, with a clear safe path**

Two distinct things:

**Conformance bitstreams (ITU/ISO/JVET/JCT-VC suites, Xiph test vectors):** These are copyrighted works with
specific distribution terms. Using them as **inputs** to our test suite is fine and is their stated purpose.
**Redistributing them inside our repo may not be.** Practical policy: do not vendor them; fetch them at test time
from their canonical URL, and gate those tests behind a `--features conformance-suite` / CI-only flag. Record the
licence of each suite in a manifest.

**FFmpeg's FATE reference files (`tests/ref/fate/*` — 2,962 files in the reference tree):** these are FFmpeg's
own outputs. A framecrc line like `0, 0, 0, 1, 152064, 0x1cbf4a06` is a **fact about FFmpeg's output**, and facts
are not copyrightable (*Feist*). But the **compilation** of 2,962 curated reference files reflects selection and
arrangement, and copying the set wholesale is a compilation-copyright risk and a terrible provenance look.

**Safe path (recommended):** our conformance tests should assert against **the spec's own defined output** or
against **freshly generated output from the installed `ffmpeg` binary at test time** (black-box differential
testing — clearly permitted observation), not against checksums lifted from FFmpeg's repo. Concretely:

```
vaco-conformance --reference $(which ffmpeg) --input tests/vectors/foo.h264 --compare md5
```

This gives us stronger testing *and* a clean provenance story, at the cost of requiring `ffmpeg` in CI (which is
fine — it's a test dependency, never linked, never shipped).

#### (e) ffprobe's XML schema and JSON field names — **AMBER, leaning GREEN**

`doc/ffprobe.xsd` defines element/type names like `ffprobeType`, `packetsType`, `streamType`, and JSON field names
like `codec_name`, `codec_long_name`, `nb_frames`, `avg_frame_rate`, `bit_rate`, `sample_aspect_ratio`.

This is the closest thing in the project to an *Oracle v. Google* fact pattern: a **naming and structural scheme**
that third-party tools depend on. Analysis:

- Individual field names are short phrases → not copyrightable individually.
- The **selection and arrangement** of ~200 field names into a nested schema is the part with any colourable claim
  — analogous to the Java package SSO the Fed. Cir. found copyrightable in 2014.
- Our use is the *Google v. Oracle* fair-use case almost exactly: we copy the naming scheme **only so far as
  necessary** for the enormous body of existing tooling and scripts that parse `ffprobe -print_format json` output
  to keep working. That is transformative-in-context, minimal, and does not substitute for FFmpeg in a market
  FFmpeg monetises (FFmpeg does not license this).

**Verdict: reimplement the field names and JSON/XML output shape. GREEN with an asterisk.**
**But: do not copy the `.xsd` file itself.** Generate our schema from our own Rust types (e.g. `serde` +
a schema generator). Do not copy the XSD's `<xsd:documentation>` prose. Do not copy the `targetNamespace`
`http://www.ffmpeg.org/schema/ffprobe` — that is FFmpeg's namespace and using it is arguably a trademark/origin
problem (§3); use `http://vaco.dev/schema/probe` and offer the FFmpeg namespace only behind an explicit
`-compat ffmpeg` flag if downstream tooling demands it (flag for counsel).

> **Lawyer question #3 (§5.4):** the ffprobe schema / JSON field-name position, specifically whether emitting the
> FFmpeg XML namespace URI under a compatibility flag is acceptable.

### 1.6 The engineering policy (adopt this verbatim)

#### 1.6.1 What a contributor may open

**Tier A — open freely, no record needed:**
- Published standards: ITU-T H.26x, ISO/IEC (14496-x, 23008-x, 23090-x, 11172-x, 13818-x), IETF RFCs, SMPTE, AES,
  ETSI/3GPP, W3C, Matroska/WebM specs, Xiph specs
- Academic papers, textbooks, conference proceedings
- Permissively-licensed reference implementations **that we have separately cleared in the crate/vendor register**
  (libopus BSD, libvpx BSD, dav1d BSD, HM BSD-3, JPEG XL Apache-2.0…) — *but see the licence-attribution duty in §4*
- FFmpeg's **user documentation**, man pages, and `--help` output
- The `ffmpeg`/`ffprobe`/`ffplay` **binaries**, for black-box testing
- Our own dirty-team specification documents

**Tier B — open only if you are on the dirty team for that module, and it must be logged:**
- FFmpeg / libav / VLC / GStreamer / mpv C source
- x264 / x265 / any GPL codec source
- Disassembly of proprietary codecs

**Tier C — never, by anyone, ever:**
- Leaked or unlawfully obtained proprietary source (Windows, QuickTime, DivX, Dolby, DTS internals)
- Anything under NDA that a contributor obtained through employment
- **AI coding assistants pointed at FFmpeg source, or prompted to "port this C to Rust"** (see §1.6.4)

**Contamination rule:** a contributor who reads Tier B material for module *X* is thereafter a **dirty-team member
for module X** and may not commit implementation code to module X. They remain clean for every other module. This
is narrower and far more workable than "once dirty, always dirty" — and it is the rule ReactOS effectively arrived
at after its 2006 audit, which suspended development for over a year and required a full codebase rewrite of
non-compliant code
(<https://www.linux.com/news/reactos-suspends-development-source-code-review/>).

#### 1.6.2 The spec-first workflow

```
  ┌──────────────────────────────────────────────────────────────────────┐
  │ 1. SOURCE            Public spec (ITU/ISO/RFC) — preferred           │
  │                      OR  dirty-team reverse engineering — exception  │
  └────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ 2. SPEC DOCUMENT     spec/<format>.md, written by the spec author.   │
  │                      Prose + our own pseudocode + tables with        │
  │                      clause-level citations. NO C. NO comments       │
  │                      copied. NO identifiers copied.                  │
  └────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ 3. GATEKEEPER REVIEW A named reviewer signs off that the spec doc    │
  │                      contains facts, not expression. Recorded in     │
  │                      the PR.                                         │
  └────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ 4. IMPLEMENTATION    A clean implementer writes Rust from the spec    │
  │                      doc + the public standard only.                 │
  └────────────────────────────────┬─────────────────────────────────────┘
                                   ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ 5. DIFFERENTIAL TEST Black-box compare against the ffmpeg binary.     │
  │                      Behaviour convergence is fine and expected —     │
  │                      that is conformance, not copying.                │
  └──────────────────────────────────────────────────────────────────────┘
```

#### 1.6.3 How we evidence it

**(a) Commit trailers.** Every commit that adds or modifies codec/format logic carries:

```
Signed-off-by: Jane Doe <jane@example.com>
Vaco-Provenance: spec
Vaco-Spec-Ref: ITU-T H.264 (08/2021) §9.3.1.1, Table 9-12
Vaco-Clean-Room: yes
Reviewed-by: John Smith <john@example.com>
```

`Vaco-Provenance` is one of: `spec` | `rfc` | `paper` | `blackbox` | `cleanroom-doc:<path>` | `original`.
`Vaco-Clean-Room: yes` is the contributor's attestation that they have not read FFmpeg (or other Tier-B) source
for this module. We adopt the **Developer Certificate of Origin 1.1** verbatim
(<https://developercertificate.org/>) and add one project-specific clause covering the clean-room attestation.
The DCO is the right model precisely because it was created for this problem — Linux adopted it in May 2004 in
response to the SCO litigation's provenance allegations
(<https://wiki.linuxfoundation.org/dco>).

**(b) PR checklist** (enforced by a GitHub PR template + a CI lint that fails on missing trailers):

```markdown
- [ ] I have NOT read FFmpeg/libav/x264/x265/VLC/GStreamer source for the module(s) this PR touches.
- [ ] Every constant table added cites the specification clause it was transcribed from.
- [ ] No table was copied from another implementation's source (including permissively-licensed ones)
      without being recorded in `THIRD_PARTY.md` with its licence.
- [ ] No text (comments, help strings, docs) was copied from FFmpeg or from a standards document.
- [ ] Tests compare against spec-defined output or a freshly-run reference binary, not against
      checksums copied from another project's repository.
- [ ] `Vaco-Provenance:` trailer present on every commit.
- [ ] If any Tier-B material was consulted: I am the dirty-team member for this module and I have
      NOT authored implementation code here.
```

**(c) Provenance records.** A `provenance/` directory, one file per format:

```yaml
# provenance/h264.yaml
format: H.264 / MPEG-4 AVC
primary_sources:
  - id: ITU-T H.264 (V15) 08/2021
    obtained: purchased from ITU, receipt 2026-03-04
  - id: ISO/IEC 14496-10:2022
clean_room_required: false        # public spec exists
spec_document: spec/h264.md
spec_author: jane@example.com
gatekeeper: john@example.com
implementers: [alice@example.com, bob@example.com]
implementers_attested_clean: true
tables:
  - name: CABAC_INIT_I
    source: ITU-T H.264 Table 9-12 .. 9-23
    method: transcribed-from-spec
  - name: ZIGZAG_4x4
    source: ITU-T H.264 Table 8-13
    method: generated-at-build-time
```

**(d) Per-file SPDX headers + REUSE compliance** (<https://reuse.software/spec-3.3/>):

```rust
// SPDX-FileCopyrightText: 2026 The vaco authors
// SPDX-License-Identifier: MIT OR Apache-2.0
```

**(e) Contributor register.** A private record of who is dirty for which module, with dates. Keep it; it is the
artefact that makes the whole system provable. Wine, Samba and ReactOS all maintain the equivalent, and the
community norm there is that having merely *looked* at leaked proprietary source disqualifies a person from
contributing thereafter (<https://forum.winehq.org/viewtopic.php?t=7138&start=25>).

#### 1.6.4 AI coding assistants — an explicit rule, because this is 2026

LLMs trained on GitHub have FFmpeg in their training data. "Write me a Rust H.264 CABAC decoder" can produce
output that is a near-verbatim translation of `libavcodec/h264_cabac.c` without the contributor ever knowing.
This is the largest *new* contamination vector and our policy must name it:

- **Forbidden:** pasting FFmpeg source into any assistant; prompting an assistant to port, translate, or
  "convert this C to Rust"; asking "how does FFmpeg implement X?"
- **Permitted:** using an assistant with our own spec document as context; asking for idiomatic Rust for a
  described algorithm; refactoring/reviewing our own code.
- **Required:** commits with substantial AI-generated codec logic carry `Vaco-Provenance: spec` **plus**
  `Vaco-AI-Assisted: yes`, and get an extra human review specifically checking for tell-tale FFmpeg idioms
  (`ff_`/`av_` prefixes, `AVCodecContext`-shaped structs, FFmpeg's characteristic macro style, its error codes).
- Run a similarity check in CI: a diff-based scan of our source against a local FFmpeg checkout, flagging any
  contiguous run of >N tokens in common outside of spec-mandated tables. This is cheap and it is exactly the
  artefact you want to be able to show a court.

> This mirrors what ReedSmith describes as the modern clean-room problem — the wall is only as good as your
> control over what enters the clean side, and generative tooling routes around the wall by default.
> <https://www.reedsmith.com/our-insights/blogs/technology-law-dispatch/102nbig/clean-room-coding-in-the-age-of-ai-who-owns-the-code/>

### 1.7 Do we actually need the two-team split? — **Pragmatic recommendation**

**Recommendation: a tiered model. Spec-first for everything with a public spec (~90% of the work); formal
two-team clean room reserved for reverse-engineered formats only.**

| Tier | Applies to | Protocol | Cost |
|---|---|---|---|
| **T1 — Spec-first (default)** | H.26x, MPEG, AAC, Opus, Vorbis, FLAC, AV1, VP8/9, MP4/ISOBMFF, Matroska, MPEG-TS, WAV/RIFF, JPEG, PNG, WebP, AVIF, most of the CLI/filter surface | Public spec → spec doc → implement. Contributors attest they have not read FFmpeg for that module. No second team needed, because **nobody needs to read FFmpeg at all**. | ~0 overhead beyond the trailer + checklist |
| **T2 — Two-team clean room (exception)** | Formats with no published spec, where FFmpeg's source is the *de facto* documentation: RealVideo/RealAudio, Bink, Smacker, Duck TrueMotion, Indeo, many game/FMV codecs, ATRAC variants, some broadcast/camera formats, quirky container edge cases | Dirty reader → spec doc → gatekeeper → clean implementer. Full paperwork. | High — budget ~2–3× the engineering time |
| **T3 — Skip** | Formats where T2 cost exceeds the value | Don't implement, or shell out to a separately-distributed plugin | 0 |

**The tradeoff, stated honestly:**

*Arguments that spec-first alone is sufficient:*
- If nobody reads FFmpeg, there is no wall to build — the "clean team" is the whole team. A two-team split
  is machinery for managing contamination that we simply do not create.
- The formal split roughly doubles cost on every module it touches and is a serious recruiting handicap: most
  people who want to work on this have already read FFmpeg source at some point in their career.
- The strongest legal protections we have (*Altai* filtration, *SAS v. WPL*, the fact that the spec dictates the
  behaviour) do not depend on a two-team structure at all.

*Arguments for going formal everywhere:*
- Contributor attestations are self-reported and unverifiable. A two-team structure with a gatekeeper produces
  third-party-verifiable evidence.
- If FFmpeg's copyright holders (a large, distributed, sometimes litigious group of individual authors) ever did
  complain, the cost asymmetry is brutal — we would spend more on two weeks of discovery than on a year of clean-room
  overhead.
- Recruiting handicap cuts both ways: the module-scoped contamination rule (§1.6.1) already lets an
  ex-FFmpeg-reader work on everything else.

**Why the tiered model wins:** the residual risk in T1 is not "we accidentally copied FFmpeg" — it is
"an individual contributor lied on an attestation". That risk is managed by the CI similarity scan (§1.6.4) plus
code review, both of which we want anyway. Spending clean-room overhead on H.264 — where the ITU spec is 800 pages
of unambiguous normative pseudocode and FFmpeg's source adds nothing we need — buys almost nothing. Spending it on
Bink, where FFmpeg's source *is* the spec, buys everything.

**Non-negotiable regardless of tier:** the DCO + provenance trailers, the CI similarity scan, per-file SPDX, and
the `THIRD_PARTY.md` register. Those are cheap and they are the evidence.

---

## 2. Patents — why FFmpeg doesn't ship binaries

### 2.1 The two reasons, separated

People conflate these constantly. They are different problems with different fixes.

**Reason 1 — licence compatibility (real, but solvable, and NOT the main reason).**
FFmpeg's build is a licence lattice (`~/repos/FFmpeg/LICENSE.md`, `configure`):

- Core: **LGPL v2.1+**. Optional parts: **GPL v2+**, activated only by `--enable-gpl`, at which point the whole
  binary is GPL v2+.
- `--enable-version3` upgrades to LGPL v3 / GPL v3 for libraries like `gmp`, `libaribb24`, `liblensfun`,
  `libopencore_amrnb/wb`, `mbedtls`, `rkmpp`, plus Apache-2.0 libraries (VMAF, OpenCORE, VisualOn) which are
  **incompatible with LGPLv2.1/GPLv2** but fine with the v3 licences.
- `--enable-nonfree` (currently `decklink`, `libfdk_aac`, `libmpeghdec`) makes the result, in FFmpeg's own words,
  **"nonfree and unredistributable"** (`configure:4836`).

So "which binary do we ship?" is genuinely hard: any single build makes a licence choice that is wrong for some
users. This is a real reason FFmpeg avoids blessing one binary — but it is a reason to ship *several* binaries,
not *none*.

**Reason 2 — patents. This is the actual reason.**
FFmpeg's legal page states the project's position plainly: they are not lawyers, and
*"we have never read patents to implement any part of FFmpeg, so even if we were qualified we could not answer it."*
It goes on to warn that *"once you start trying to make money from patented technologies, the owners of the patents
will come after their licensing fees"*, naming MPEG LA specifically.
<https://www.ffmpeg.org/legal.html>

The mechanism is this: **patent pool licences are priced per unit of a distributed product.** Distributing *source
code* is generally not treated as distributing a licensable "unit" — there is no encoder or decoder until someone
compiles it. Distributing *binaries* creates units, and units create royalty obligations and a countable,
attributable act of infringement in a jurisdiction. By shipping source only, the FFmpeg project:

1. incurs no per-unit royalty liability itself;
2. never commits an act of "making, using, offering to sell, selling, or importing" a patented apparatus in the US
   (35 U.S.C. § 271) that a pool could point at;
3. pushes the compliance decision to whoever builds and distributes — who is in a position to know their own
   jurisdiction, volume, and business model.

**Corollary for us: shipping binaries is precisely the thing that converts FFmpeg's abstract patent question into
our concrete patent liability.** This is the single most important structural fact in this document.

### 2.2 Who does ship builds, and how they handle it

| Distributor | What they ship | How they handle patents |
|---|---|---|
| **BtbN** (GitHub Actions) | Static Windows/Linux `ffmpeg` builds, GPL and LGPL variants | Individual hobbyist project; no licensing programme. Operates on the "nobody sues a free build" theory. <https://github.com/BtbN/FFmpeg-Builds> |
| **gyan.dev** | Windows "essentials" and "full" builds | Same |
| **John Van Sickle** | Static Linux builds | Same |
| **Debian / Ubuntu** | `ffmpeg` in `main`, including patent-encumbered decoders (MPEG-4, MP3, H.264 decode via libavcodec) | Debian's position is essentially that it is a non-commercial distributor; it historically excluded some encoders and relies on the practical reality that pools pursue commercial device makers. Debian ships **libx264/libx265 in `main`** and takes the GPL build. |
| **Fedora / Red Hat** | `ffmpeg-free` — a **deliberately stripped** build with encumbered codecs removed | Fedora is the clearest example of a patent-conservative policy, because Red Hat is a large US commercial entity with assets. Users are pushed to **RPM Fusion**, hosted outside US jurisdiction, for the full build. Fedora *did* relax in 2021+ to ship some previously-excluded codecs (notably AV1, and H.264 decode via `openh264` from Cisco's repo) once risk assessments changed. |
| **Mozilla / Firefox** | Does not ship an H.264 encoder/decoder in its own binary | Downloads **Cisco's pre-built OpenH264 binary at runtime**. The trick: OpenH264's *source* is BSD-2-Clause, but **Cisco pays the MPEG LA/Via LA royalties on the binaries Cisco itself compiles and distributes**, and the pool's per-unit cap means Cisco's cost is bounded. Anyone who compiles OpenH264 from source is on their own for licensing. <https://blogs.cisco.com/collaboration/ciscos-openh264-now-part-of-firefox> |
| **Google / Chrome, Apple, Microsoft** | Ship everything | They are pool licensees, pay per-unit royalties (or hit the caps), and in several cases are pool *licensors* who get cross-licensed. They also lean on **hardware** decode, where the SoC vendor's licence often already covers the unit. |
| **VideoLAN (VLC)** | Ships binaries including encumbered codecs | Explicitly relies on being a **French non-profit**: *"Neither French law nor European conventions recognize software as patentable."* This is a jurisdiction-of-suit argument, not a claim that no valid European codec patents exist. <http://www.videolan.org/press/patents.html> |

**The pattern:** everyone who ships binaries either (a) is too small/non-commercial to be worth suing,
(b) is offshore of the aggressive jurisdiction, (c) pays, or (d) strips the encumbered codecs.
**Option (d) is the only one available to a project that wants to ship its own binaries, be US-reachable, and be
commercially usable by its users. That is the strategy this document recommends.**

### 2.3 Per-codec patent risk table

**How to read this table.** "Expiry status" means *last known essential patent in the major pool(s), US, as of
2026-08-21*. **Expired ≠ zero risk**: unpooled holders, continuations, and non-US patents exist for almost
everything. Verdicts are for **distributing our own binaries, worldwide, from a US/EU-reachable entity**.

- 🟢 **GREEN** — ship in the default build.
- 🟡 **AMBER** — do not ship in the default build; opt-in / build-it-yourself, or decode-only, or needs counsel.
- 🔴 **RED** — never in a distributed binary without a signed licence.

#### Video

| Codec | Pool(s) | Essential-patent status (2026-08) | Enc vs Dec | RF grant? | Verdict |
|---|---|---|---|---|---|
| **H.261** | None ever | Priority dates ≤1990 → long expired | same | n/a | 🟢 **GREEN** (enc+dec) |
| **H.262 / MPEG-2 Video** | MPEG LA → Via LA | **Last US patent expired 2018-02-13** (US 7,334,248). Programme still nominally open; Via LA lists Malaysia as the only jurisdiction with live patents. <https://www.phoronix.com/scan.php?page=news_item&px=MPEG-2-Last-Patents-Expire> <https://www.via-la.com/licensing-programs/mpeg-2/> | same | No, but moot | 🟢 **GREEN** (enc+dec). Note MPEG-2 **Systems** (TS muxing) is a separate Via LA programme — see Containers. |
| **H.263** | No modern pool; IP overlaps MPEG-4 Part 2 | 1995-era priority → expired | same | n/a | 🟢 **GREEN** |
| **MPEG-4 Part 2 (ASP, DivX/Xvid)** | MPEG LA (wound down) | US cleared Nov 2023, EU Apr 2021; **last patent worldwide (Brazil, BRPI0109962B1) reported expired 2026-07-19** — *low-confidence date, single-source* <https://xenospectrum.com/en/mpeg4-divx-xvid-patent-expires/> | same | No, but moot | 🟢 **GREEN** (verify the BR date if we sell into Brazil) |
| **H.264 / AVC** | **Via LA** (44 licensors, 1,700+ licensees). Patent list updated 2026-08-01 — **pool is active** | Not expired. Long tail runs to ~2027–2028 (commonly cited last US patent US 7,826,532, 2027-11-29). <https://www.via-la.com/licensing-programs/avc-h-264/> | **Same** — a "unit" is a product containing an encoder *or* a decoder. First **100,000 units/yr free**, then $0.20/unit (100k–5M), $0.10 above, enterprise cap ~$9.75M/yr. Separate "free internet video" carve-out for non-subscription content. | No | 🟡 **AMBER**. The 100k/yr free tier is *real* and may cover us initially — but "units" counts downloads and we would blow through it. **Not in default build.** Consider the Cisco-OpenH264 pattern (§5.2). |
| **H.265 / HEVC** | **Three-to-four pools + unpooled holders.** Access Advance **acquired Via LA's HEVC/VVC pool administration on 2025-12-15** (operated as VCL Advance / "Video Codec Licensing LLC"), consolidation targeted end-2026 but **not complete**. Plus Sisvel. Plus unpooled (Dolby/GE-heritage). <https://ipfray.com/breaking-access-advance-acquires-via-licensing-alliances-hevc-vvc-patent-pools/> <https://accessadvance.com/licensing-programs/hevc-advance/> | Far from expired (2030s) | No enc/dec distinction in either major pool. Access Advance historically charges **content-distribution royalties** too, unlike AVC — the industry's biggest complaint. Rate increase deferred: current rates locked for licensees signing by **2026-06-30** <https://accessadvance.com/2026/01/27/access-advance-extends-hevc-advance-rate-increase-deadline/> | No | 🔴 **RED**. Worst case in the entire table. Multiple pools means you can pay one and still be sued by another. |
| **H.266 / VVC** | Access Advance "VVC Advance" (launched 2021-06-30) + the ex-Via LA VVC pool, now same owner | Nowhere near expired (2040s) | Via LA's VVC terms notably distinguished **$0.20/unit paid software vs $0.05/unit free software**, cap $30M/yr, and **no royalty on encoded content** <https://www.via-la.com/hevc-vvc-tcl-2024/> | No | 🔴 **RED** |
| **AV1** | **AOMedia Patent License 1.0 (RF)** — *plus* **Sisvel's AV1 pool** (20 owners incl. Dolby, Philips, Toshiba; €0.32/€0.24 display, €0.11/€0.08 non-display; 64 licensees) | n/a — new codec | RF grant is reciprocal & royalty-free to any implementer <https://aomedia.org/license/patent-license/> | **Yes, from AOM members** — but AOM members do not own all AV1-essential patents | 🟡 **AMBER — downgraded from GREEN in March 2026.** See below. |
| **VP8** | MPEG LA pool effort **abandoned March 2013** after Google cross-licensed the 11 holders | Effectively cleared + age | same | Google RF grant + reciprocal cross-licence <https://www.webmproject.org/cross-license/vp8/agreement/> | 🟢 **GREEN** |
| **VP9** | **Sisvel VP9 pool** (launched March 2019, €0.24/€0.08) — Google/AOM say RF | Not expired | same | Google RF grant; Sisvel disputes sufficiency | 🟡 **AMBER** (lower risk than AV1 in practice — less deployed, less worth suing over — but same structural defect) |
| **AVS2 / AVS3** | AVS Patent Pool (China, est. 2004) | Active | unknown | No | 🟡 **AMBER** — decode-only, and only if a user asks. Rate structure and non-China enforcement posture **not verifiable from English sources**. |
| **ProRes** | No pool. Apple runs a private **authorisation/certification programme** (`ProRes@apple.com`). Apple's support page explicitly names *"FFmpeg and derivative implementations"* as unauthorised. <https://support.apple.com/en-us/118584> | No public essential-patent list exists | Apple's objection is aimed at **encoders** | No | **Decode 🟡 AMBER / Encode 🔴 RED.** No known Apple patent assertion against FFmpeg, but "no public patent list" is not "no patents", and the trademark/certification angle (§3) is independently live. |
| **DNxHD / VC-3** | SMPTE standard, but **an Avid patent licence is reportedly still required for commercial use** (fee schedule not public) <https://en.wikipedia.org/wiki/Avid_DNxHD> | Unclear | Unclear | No | 🟡 **AMBER** — decode probably fine, encode needs counsel |
| **CineForm** | Open-sourced by GoPro Oct 2017, **dual MIT/Apache-2.0** — Apache-2.0 carries an express patent grant <https://fstoppers.com/news/gopro-open-sources-cineform-codec-201183> | n/a | same | Yes (via Apache-2.0 §3) | 🟢 **GREEN** |
| **Screen/game codecs** (MS Screen 1/2, TSCC, ZMBV, QuickTime RLE, Bink, Smacker, Duck TrueMotion, Indeo, Cinepak) | None found | 1990s-era | same | ZMBV is open by design; TSCC was freely distributed; RAD/Epic's stated policy is **never to patent** its codec tech <https://en.wikipedia.org/wiki/Bink_Video> | 🟢 **GREEN on patents** — but most are **T2 clean-room** (§1.7) because no spec exists. That is the binding constraint here, not patents. |

**⚠ AV1 — the March 2026 development that changes the calculus.**
On **2026-03-23** Dolby Laboratories sued Snap Inc. in **D. Del. (1:26-cv-00317)** and in Brazil over four video
compression patents (acquired from GE in June 2024) reading on **both AV1 and HEVC**, seeking an **injunction**.
Dolby never joined AOMedia, so it has **no FRAND and no royalty-free commitment** and is free to seek injunctive
relief. Access Advance's own statement is blunt: *"Labeling a codec 'royalty-free' does not eliminate underlying
patent rights."* InterDigital also has a pending AV1 assertion against Amazon Fire devices.
<https://accessadvance.com/2026/03/24/access-advance-licensor-sues-snap-inc-for-av1-and-hevc-patent-infringement/>
<https://ipfray.com/dolby-sues-snapchat-over-av1-and-hevc-patent-infringement-in-u-s-and-brazil-access-advance-vdp-license-would-resolve-issue/>
<https://streaminglearningcenter.com/encoding/when-theres-no-frand-what-dolbys-suit-against-snap-means-for-the-industry.html>

This does **not** mean AV1 is unusable. It means AV1's "royalty-free" status protects you from *AOM members*, not
from the world, and the 2026 litigation trend is upward. **My recommendation is still to ship AV1** (see §5.1) —
it remains the best available option and the AOM grant plus AOM's collective defensive posture is real value — but
it should be understood as an **AMBER-with-a-good-story**, not a GREEN, and the position should be revisited when
*Dolby v. Snap* resolves. This is **lawyer question #4**.

#### Audio

| Codec | Pool(s) | Status (2026-08) | Enc vs Dec | RF? | Verdict |
|---|---|---|---|---|---|
| **MPEG-1/2 Layer II** | ex-MPEG LA | Predates Layer III → expired | same | — | 🟢 **GREEN** |
| **MP3 (Layer III)** | Fraunhofer IIS / Technicolor | **Programme terminated 2017-04-23**; last US patent (US 6,009,399) expired **2017-04-16** <https://www.iis.fraunhofer.de/en/ff/amm/consumer-electronics/mp3.html> | same | — | 🟢 **GREEN** (enc+dec) |
| **AAC-LC / HE-AAC / HE-AACv2 / xHE-AAC (USAC) / AAC-LD/ELD** | **Via LA AAC pool — ACTIVE**, licensors incl. Dolby, Fraunhofer, Philips, Sony, ETRI, Orange, VoiceAge <https://www.via-la.com/licensing-programs/aac/> | Not expired. AAC-LC core patents are 1990s-filed and a large fraction have lapsed, but the pool has **not** wound down and HE/xHE are much newer. | **Royalty on encoder/decoder sale only — explicitly NO royalty on the bitstream**: *"no patent license fees due for the distribution of bit-streams encoded in AAC, whether broadcast, streamed over a network, or provided on physical media"* <https://via-la.com/licensing-2/aac/aac-faqs> | No | 🔴 **RED for a distributed binary.** The pool is alive and Fraunhofer/Via LA actively license software. This is the most painful exclusion because AAC is unavoidable in MP4. See §5.2 for the mitigation. |
| **AC-3 (Dolby Digital)** | Dolby direct | **Last patent expired 2017-03-20** <https://freetoairamerica.wordpress.com/2017/03/20/electronic-frontier-foundation-the-patent-on-dolby-digital-ac-3-has-just-expired/> | same | — | 🟢 **GREEN on patents.** ⚠ The **"Dolby Digital" trademark and certification programme are separate and still live** — implement the bitstream, do not use the mark (§3). |
| **E-AC-3 (Dolby Digital Plus)** | Dolby direct | **Last patent (US 7,516,064) adjusted expiry 2026-01-30** — i.e. ~7 months ago. Reported by Phoronix reading Google Patents, hedged as *"might now be expired"*; **no official Dolby statement found**. <https://www.phoronix.com/news/Dolby-Digital-Plus-E-AC3-2026> | same | — | 🟡 **AMBER → likely GREEN.** Single-source, very recent, load-bearing. **This is worth a paid patent-landscape search before we rely on it** — but if it holds, E-AC-3 decode becomes shippable, which matters a lot for streaming content. Same trademark caveat as AC-3. |
| **AC-4** | Dolby direct, no pool | Current | Consumer device $0.15–$1.20/unit; content encoding free. *Rate figures single-source, unverified.* | No | 🔴 **RED** |
| **DTS family (DTS, DTS-HD, DTS:X)** | Xperi/DTS direct, no pool, rates not public <https://dts.com/patents/> | Current | Per-unit to manufacturers; mandatory for Blu-ray | No | 🔴 **RED** |
| **Dolby Vision** | Dolby direct + requires the **base-layer codec licence too** (usually HEVC → RED) | Current | Certification required | No | 🔴 **RED** (double-encumbered) |
| **Opus** | None | RF by design. RFC 6716 (2012-09-11). Xiph, Broadcom and **Microsoft (Skype/SILK)** filed royalty-free IPR disclosures. Qualcomm, Huawei, France Telecom and Ericsson filed *potentially royalty-bearing* disclosures that were reviewed and not considered blocking. <https://opus-codec.org/license/> | same | **Yes** | 🟢 **GREEN** — the single best audio choice we have |
| **Vorbis** | None | Xiph, RF by design | same | Yes | 🟢 **GREEN** |
| **FLAC** | None | Xiph, RF by design <https://xiph.org/flac/license.html> | same | Yes | 🟢 **GREEN** |
| **ALAC** | None | Apple open-sourced 2011-10-27 under **Apache-2.0** → express patent grant from Apple <https://appleinsider.com/articles/11/10/28/apple_lossless_audio_codec_project_becomes_open_source> | same | Yes (Apache §3) | 🟢 **GREEN** |
| **Speex** | None | Xiph, RF; superseded by Opus | same | Yes | 🟢 **GREEN** |
| **AMR-NB** | 3GPP SEP landscape; no clean public pool found | ~1999 priority → largely expired | same | No | 🟡 **AMBER** (decode probably fine; gap in research) |
| **AMR-WB / G.722.2** | **VoiceAge-administered pool**, launched 2010-02-01 (Ericsson, Orange, Nokia, VoiceAge) | Active-ish; core patents ~2002 priority, tail unclear | same | No | 🟡 **AMBER** |
| **EVS** | **Via LA "Voice Codec" programme** (bundles EVS + IVAS; Dolby, ETRI, Huawei, NTT, JVCKenwood) — **actively growing in 2026** <https://www.via-la.com/licensing-programs/voice-codec/> | Current | FRAND | No | 🔴 **RED** |
| **G.711, G.722** | None | 1972/1988 → long expired | same | — | 🟢 **GREEN** |
| **G.726** | None found | ~1990 → presumed expired (*not individually verified*) | same | — | 🟢 **GREEN** (low confidence on citation, high confidence on substance) |
| **G.729 / G.729.1 / G.711.1** | Sipro Lab Telecom | **Royalty-free from 2017-01-01** by agreement of Orange, NTT, U. Sherbrooke <https://www.mgraves.org/2017/03/its-official-the-patents-on-g-729-have-expired/> | same | Now yes | 🟢 **GREEN** |
| **G.723.1** | Sipro Lab Telecom (same pool) | Expiry date **not found** — presumed expired by age | same | — | 🟡 **AMBER** (verify) |

#### Image / still

| Format | Pool(s) | Status | RF? | Verdict |
|---|---|---|---|---|
| **JPEG (baseline)** | None. Forgent Networks' '672 patent troll campaign (2004–07) collapsed — USPTO rejected 19 of 47 claims; patent expired Oct 2006 <https://www.infoworld.com/article/2174792/patent-office-rejects-forgent-s-jpeg-claims.html> | Clear | — | 🟢 **GREEN** |
| **JPEG 2000** | None active. Part 1 developed under ITU-T patent policy 2.1 ("fee-free" commitments from *disclosed* holders) | Best-effort disclosure regime, **not a guarantee** <https://wiki.endsoftwarepatents.org/wiki/JPEG_2000> | Soft yes | 🟢 **GREEN** (mildly qualified) |
| **JPEG XL** | Google RF grant covering patents necessarily infringed by the reference implementation, with defensive termination <https://github.com/libjxl/libjxl/blob/main/PATENTS> | Single-contributor grant (AOM-shaped), not a committee-wide FRAND commitment | Yes, from Google | 🟢 **GREEN** |
| **PNG / GIF / WebP / BMP / TIFF** | None (LZW expired 2004) | Clear | — | 🟢 **GREEN** |
| **HEIF (container)** | Nokia grants RF for its reference software but **expressly excludes "Codec Patents"** <https://github.com/nokiatech/heif/wiki/VI.-License> | Container clear; **payload is HEVC in ~every real file** | Container only | 🔴 **RED in practice** — the container is fine, the content isn't. Parse the container, refuse the payload. |
| **AVIF** | Inherits AV1's AOM licence — and AV1's exposure | See AV1 | Yes (AOM) | 🟡 **AMBER** (same posture as AV1) |

#### Containers / systems

| Format | Note | Verdict |
|---|---|---|
| **MPEG-2 Systems (TS/PS)** | **Separate Via LA programme from MPEG-2 Video**, and it outlived the video patents. Check current status before shipping a TS muxer commercially. <https://www.via-la.com/licensing-2/mpeg-2-systems/> | 🟡 **AMBER** — verify. Demux is lower risk than mux. |
| **MP4 / ISOBMFF** | ISO/IEC 14496-12. No pool. | 🟢 **GREEN** |
| **Matroska / WebM / MKV** | Open spec, RF | 🟢 **GREEN** |
| **ASF / WMV container** | FFmpeg's own FAQ notes *"Microsoft claims a patent on the ASF format, and may sue"* (`doc/faq.texi`). Age (1990s) makes this likely stale, but it is FFmpeg's own recorded warning. | 🟡 **AMBER** (demux only) |
| **Ogg, WAV/RIFF, AIFF, FLAC, CAF, MXF, AVI** | No known encumbrance | 🟢 **GREEN** |

### 2.4 Decoders vs encoders; software vs hardware

**Decode vs encode — the honest answer is that the *licences* mostly don't distinguish, but the *risk* does.**

- **In the licence text:** Via LA's AVC licence defines a "unit" as a product containing an encoder **or** a
  decoder — same price either way. HEVC pools are the same. So there is **no contractual safe harbour for
  decode-only**.
- **In practice, decode-only is materially lower risk**, for four reasons:
  1. Encoders are what commercial video businesses buy; that is where the money and therefore the enforcement is.
  2. Encoder patents (rate control, motion estimation, mode decision) are often *newer* than the bitstream-syntax
     patents, so the encoder tail outlives the decoder tail.
  3. A decoder implements only what the spec mandates — a much stronger *scènes à faire* and merger story on the
     copyright side too.
  4. Apple's ProRes objection, Dolby's certification programmes, and Fraunhofer's AAC posture are all
     encoder-focused in practice.
- **Our policy:** where a codec is AMBER, ship **decode only** and gate encode behind opt-in. Where a codec is
  RED, ship neither.

**Software vs hardware.**
- **Hardware** decode/encode via VA-API, VideoToolbox, D3D11VA, NVDEC/NVENC, AMF is a genuinely different posture:
  the patent licence for the codec is typically **already paid by the SoC/GPU vendor** and embodied in the device
  the user already owns. Our binary is then a *client of a licensed decoder*, not itself a decoder.
- **This is the single most valuable mitigation available to us.** For HEVC, VVC, and AVC, a hardware-only path
  (`vaco -hwaccel auto -c:v hevc_videotoolbox`) lets users decode and encode encumbered formats using the licence
  already embedded in their hardware, while our shipped binary contains **no software implementation of the
  codec at all**.
- It is not a complete answer — counsel should confirm that facilitating use of a licensed hardware decoder
  creates no inducement/contributory infringement exposure (35 U.S.C. § 271(b)/(c)). This is **lawyer question #5**.
- **System codecs** (Media Foundation on Windows, AVFoundation/VideoToolbox on macOS, MediaCodec on Android) are
  the same argument in software form: the OS vendor is licensed.

### 2.5 The AOMedia patent licence and OIN — what they actually buy

**Alliance for Open Media Patent License 1.0** <https://aomedia.org/license/patent-license/>
- Grants a *"non-sublicensable, perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable patent
  license"* over **Necessary Claims** to implement the AV1 specification.
- **Available to anyone**, member or not — you do not need to join AOMedia to receive it.
- **Reciprocity condition:** to benefit, the licensee must make its own Necessary Claims available under the same
  licence, and must reproduce the licence with any distributed implementation.
- **Defensive termination:** if the licensee initiates patent litigation against another party regarding an AV1
  implementation, *"any patent licenses granted under this License directly to the Licensee are immediately
  terminated"* — with carve-outs for counterclaims and for enforcing the licence itself.

**What it does buy us:** immunity from every AOM member's AV1-essential patents (Google, Amazon, Apple, ARM,
Cisco, Intel, Meta, Microsoft, Mozilla, Netflix, Nvidia, Samsung, Tencent, and ~50 more) at zero cost, without
joining anything. That is a very large and very real shield.

**What it does not buy us:** anything at all against **Dolby, Sisvel's licensors, InterDigital, Nokia**, or any
other non-member. *Dolby v. Snap* (§2.3) is that gap being exercised for the first time.

**Should we join AOMedia?** Probably not worth it initially — membership fees are substantial and **the licence
is available without membership**. Revisit if we ever hold patents (we won't) or want a seat in the spec process.

**Open Invention Network (OIN)** <https://openinventionnetwork.com/linux-system/>
- OIN runs a **royalty-free cross-licence among ~4,000 members** covering a defined "Linux System" package list,
  which **expressly includes a list of audio, video and still-image codecs**.
- Members agree not to assert their patents against Linux System components. Joining is **free**.
- **What it buys:** non-aggression from fellow members — which includes a lot of large patent holders.
- **What it does not buy:** anything against non-members. It would not have stopped *Dolby v. Snap*.
- **Recommendation: join. It is free, it is a positive signal, and it costs us nothing** (we hold no patents to
  give up). But do not treat it as patent clearance. Confirm whether our specific codec set falls inside the
  current Linux System Definition, which is revised roughly every two years.

### 2.6 Does reimplementing in Rust change patent exposure? — **No. Plainly, no.**

**A patent covers a method or apparatus, not a source text.** 35 U.S.C. § 271(a): infringement is to
*"make, use, offer to sell, or sell"* the patented invention. A patent claiming "a method of entropy-decoding a
video bitstream comprising..." is infringed by **any** implementation that performs those steps — in C, Rust,
assembly, Verilog, or by hand on paper.

There is **no** language-based defence, **no** clean-room defence (clean rooms address copyright only), and **no**
independent-invention defence in US patent law. Independent creation is a complete defence to copyright
infringement and **no defence at all** to patent infringement.

Restating for the record, because this misconception is common and expensive:

> **A clean-room Rust reimplementation of FFmpeg has exactly the same patent exposure as FFmpeg itself.
> Our entire patent strategy must therefore be codec selection and distribution structure — not engineering.**

The corollary is mildly encouraging: because the exposure is identical, every mitigation that works for other
distributors (§2.2) works for us, and we get to *choose* our codec set from a clean sheet rather than inheriting
FFmpeg's twenty years of accumulated encumbrance.

### 2.7 Jurisdictional differences and enforcement reality

**US.** Software-implemented methods are patentable subject matter subject to *Alice Corp. v. CLS Bank*,
573 U.S. 208 (2014), and 35 U.S.C. § 101. **Codec patents survive *Alice* routinely** — they claim concrete signal
processing with specific technical steps, not abstract ideas. The US is the aggressive forum: high damages,
willfulness/treble damages under § 284, and (post-*eBay*) injunctions are harder but still available, especially
to non-competitors like Dolby suing Snap.

**EU.** EPC Art. 52(2)(c)/(3) excludes "programs for computers **as such**", but the EPO's "further technical
effect" doctrine has narrowed that exclusion to near-nothing for signal processing. **Video and audio compression
patents issue and are enforced in Europe as a matter of routine.** The **Unified Patent Court** (operational since
June 2023) has made pan-European injunctions faster and cheaper to obtain, which cuts *against* the old
"Europe is safe" intuition. The Munich courts in particular have granted injunctions to SEP holders (e.g. Nokia
obtained a Germany-wide injunction against Amazon's Fire TV Stick).

> **"Software isn't patentable in Europe" is not a safe simplification, and we should not build a strategy on it.**
> VideoLAN's position (<http://www.videolan.org/press/patents.html>) is really a *jurisdiction-of-suit* argument —
> a French non-profit with no US assets is hard to reach — not a claim that no valid European codec patents exist.
> We should not assume we can replicate VideoLAN's posture unless we are willing to replicate its corporate
> structure and forgo US commercial activity.

**Enforcement reality for open source.** The empirical record is genuinely reassuring, with one important caveat:

- **No evidence of MPEG LA / Via LA / Access Advance / Sisvel ever suing the FFmpeg project, VideoLAN, or x264
  directly.** The only suit found against x264/FFmpeg was an obscure 2011 action by "Dideonet" over a
  parallel-processing patent, outcome unknown
  (<https://ffmpeg.org/pipermail/ffmpeg-devel/2011-October/115983.html>).
- The strategic logic is obvious: pools monetise **device makers and commercial distributors**, not upstream FOSS
  projects with no revenue. Suing a beloved non-profit is bad PR and yields nothing.
- **The caveat, and it is the whole point:** *Dolby v. Snap* shows that holders absolutely will sue a
  **well-resourced commercial user** of a "royalty-free" codec. Our users are exactly that population. If `vaco`
  becomes the thing that shipped HEVC into a thousand commercial products, our users get sued, and we get named as
  a contributory infringer. **The risk is not to the project; it is to the project's users, and therefore
  to the project's reputation and to any commercial entity behind it.**

---

## 3. Trademark

### 3.1 What FFmpeg claims

FFmpeg's legal page states: **"FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project."**
<https://www.ffmpeg.org/legal.html>

Two observations:

1. This is a **unilateral assertion**. I found **no confirmation of a USPTO or EUIPO registration**, and I did not
   run a live database search. **Unconfirmed — do a TESS/EUIPO/WIPO Global Brand Database search before relying on
   registration status either way.** Note that in the US, **unregistered common-law trademark rights arise from
   use** under Lanham Act § 43(a), 15 U.S.C. § 1125(a), so the absence of a registration does not mean the absence
   of rights. FFmpeg has used the mark continuously since 2000 in a well-defined market. Assume the mark is
   enforceable.
2. FFmpeg's own naming guidance (via the FFmpegKit wiki, reflecting project guidance) asks that the name be spelled
   correctly (`FFmpeg` — two capital Fs, lowercase `mpeg`) and that FFmpeg DLLs not be renamed to obfuscate their
   origin (`avcodec-MyProg.dll` acceptable; `MyProgDec.dll` not). That guidance is aimed at *users of FFmpeg*, not
   at reimplementers, but it tells us the project cares about origin attribution.

### 3.2 The critical distinction: naming the *project* vs naming the *binary*

These are genuinely different risks and the project should treat them differently.

| | Risk | Verdict |
|---|---|---|
| **Calling the project "FFmpeg"** (or "FFmpeg-rs", "RustFFmpeg", "FFmpeg Reborn", "NeoFFmpeg") | **Highest risk.** This is classic source-identifying use likely to cause confusion as to origin, sponsorship or affiliation — the core Lanham Act § 32/§ 43(a) claim. Also almost certainly what the mark holder would object to first. | 🔴 **NEVER.** Not negotiable. |
| **Using the FFmpeg logo** (the zigzag/raster "ff" mark) | Logos are strong source identifiers; no licence has been granted to us; no fair-use theory covers decorative reuse. | 🔴 **NEVER.** |
| **Shipping a binary literally named `ffmpeg` / `ffprobe` / `ffplay`** | **The genuinely interesting case.** See §3.3. | 🟡 **AMBER** — do it via an opt-in compatibility mechanism, not as the primary artefact. |
| **Saying "drop-in replacement for ffmpeg", "FFmpeg-compatible CLI", "accepts the same options as ffmpeg"** | **Nominative fair use.** *New Kids on the Block v. News America Publishing*, 971 F.2d 302 (9th Cir. 1992): (1) the product is not readily identifiable without the mark; (2) only so much of the mark as is reasonably necessary is used; (3) nothing suggests sponsorship or endorsement. *Toyota v. Tabari*, 610 F.3d 1171 (9th Cir. 2010) applied this even to domain names. There is no way to describe "we implement the same CLI" without naming FFmpeg. | 🟢 **GREEN** — do this freely, with a disclaimer. |
| **Using `http://www.ffmpeg.org/schema/ffprobe` as our XML namespace** | Uses FFmpeg's domain as an origin identifier in our output. Weak trademark argument on its own (namespaces are identifiers, not brands) but bad practice and an unnecessary fight. | 🟡 Use our own namespace; offer FFmpeg's only behind `-compat ffmpeg` if downstream tooling truly requires it. Flag for counsel. |

### 3.3 The `ffmpeg`-named binary question

This is the one that matters operationally, because the value proposition of `vaco` is that ten million existing
shell scripts, Dockerfiles, CI pipelines and `subprocess.run(["ffmpeg", ...])` calls keep working.

**The argument that it is fine:** a binary filename on disk is a functional identifier used by an operating
system's `$PATH` resolution, not a brand presented to a consumer at the point of purchase. Nobody is confused
about who made `vaco` when they install `vaco`. Compare `sh`, `awk`, `cc`, `vi`, `tar` — all reimplemented under
their canonical names for decades without trademark objection. Debian's `update-alternatives` mechanism exists
precisely to let multiple implementations occupy one command name.

**The argument that it is not fine:** trademark law protects against confusion as to **origin, sponsorship, or
affiliation**, and a binary that identifies itself as `ffmpeg`, prints a banner, and answers `--version` is
making a source-identifying representation. And the precedent among comparable projects is uniformly one of
**renaming**:

- **Libav (2011)** — the FFmpeg fork renamed its binary from `ffmpeg` to **`avconv`**, specifically to distance
  itself from the FFmpeg name, even though it was a direct fork with a legitimate claim to the codebase's history.
  <https://blog.pkh.me/p/13-the-ffmpeg-libav-situation.html>
- **LibreSSL (2014)** — OpenBSD's OpenSSL fork renamed. OpenSSL's trademark policy expressly forbids use of
  "OpenSSL" in a product name or anything confusingly similar.
  <https://openssl-library.org/policies/general/trademarkpolicy/>
- **Iceweasel (2006–2016)** — Debian shipped patched Firefox under a different name because Mozilla's trademark
  policy required unmodified binaries or prior approval.
  <https://en.wikipedia.org/wiki/Debian%E2%80%93Mozilla_trademark_dispute>

Three separate projects, three different mark holders, one consistent outcome. That is a strong signal.

### 3.4 Recommended naming policy

1. **The project, the crates, the primary binaries, and the brand are `vaco` / `vaco`, `vaco-probe`, `vaco-play`.**
   No FFmpeg-derived name anywhere in the identity.
2. **Compatibility shims are shipped, but as an explicit opt-in**, not as the default install:
   - `vaco compat install` (or a separate `vaco-compat` package) creates `ffmpeg`/`ffprobe`/`ffplay` symlinks or
     shim scripts pointing at `vaco`.
   - The shim prints, on first use in an interactive terminal:
     `note: 'ffmpeg' here is a compatibility shim for vaco. vaco is not FFmpeg and is not affiliated with the FFmpeg project.`
   - Distro packagers can wire this through `update-alternatives` / Homebrew `link --overwrite` at the *user's*
     election, which puts the naming decision on the person best placed to make it.
3. **`vaco --version` never claims to be FFmpeg.** It may report a *compatibility level*
   (`vaco 0.4.0 (ffmpeg-cli compatibility: 7.1)`), which is descriptive, factual and nominative.
   - ⚠ **Do not emit a fake `ffmpeg version 7.1` banner** even if scripts parse for it. If a real-world script
     genuinely requires that string, gate it behind `--compat-version-string` with a documented warning. Flag for
     counsel.
4. **Never use the FFmpeg logo, wordmark styling, or colour scheme.** Commission our own.
5. **Standard disclaimer** in README, docs footer, and website:
   > vaco is an independent, clean-room implementation. It is not affiliated with, endorsed by, or derived from
   > the FFmpeg project. FFmpeg is a trademark of Fabrice Bellard.
6. Same analysis applies to **third-party marks in codec names**: `Dolby Digital`, `DTS`, `Dolby Vision`,
   `ProRes`, `DivX` are all trademarks with certification programmes attached. **Use the technical designators**
   (`ac3`, `eac3`, `dts`, `prores`) as codec identifiers — descriptive/nominative use — and never claim
   certification, compliance, or the branded names in marketing.
   This matters most for **AC-3/E-AC-3**, where the patents have expired but the trademark has not (§2.3).

> **Lawyer question:** whether the opt-in `ffmpeg` shim, and the `ffmpeg-cli compatibility: 7.1` version string,
> are acceptable. My read is that both are defensible; the fake-banner option is not.

---

## 4. Licence compatibility for an MIT (→ MIT OR Apache-2.0) deliverable

### 4.0 The trap that will bite us first

**`crates.io` metadata describes the Rust wrapper, not the C library it statically links.** This is not a
theoretical concern — it is the single most likely way we ship a GPL binary by accident. Verified examples
(queried from the crates.io API, 2026-08-21):

| Crate | Declared crates.io licence | What it actually links | Effective licence of our binary |
|---|---|---|---|
| `x264` v0.5.0 | **`MIT`** | libx264 — **GPL-2.0-or-later** | 🔴 **GPL** |
| `x265` v0.1.1 | **`MIT`** | libx265 — **GPL-2.0-or-later** | 🔴 **GPL** |
| `freetype-sys` v0.23.0 | **`MIT`** | FreeType — **FTL or GPL-2.0** dual | FTL (attribution duty) or GPL |
| `libwebp-sys` v0.14.4 | `MIT` | libwebp — BSD-3-Clause | BSD-3 (fine, but the metadata is still wrong) |
| `libass-sys` v0.1.2 | `ISC` | libass — ISC | ✅ correct, by luck |

`cargo-deny` reads the declared metadata and will **cheerfully pass a GPL-linked binary**. Therefore:

> **Policy: every `*-sys` crate and every crate with a `build.rs` that compiles or links C/C++ must have a manual
> entry in `THIRD_PARTY.md` recording the *upstream library's* licence, and must be added to a
> `deny.toml` review list. Automated tooling is necessary but not sufficient.**

### 4.1 Crate assessment

Licences below were queried directly from the crates.io API on **2026-08-21** (latest published version).
✅ = usable in the default MIT/Apache-2.0 binary. ⚠️ = usable with a condition. 🔴 = not in the default build.

#### AV1

| Crate / lib | SPDX | Pure Rust? | Verdict |
|---|---|---|---|
| `dav1d` v0.11.1 (bindings) | `MIT` | FFI → **dav1d, BSD-2-Clause** | ✅ Best AV1 decoder. Note: FFI. |
| `rav1e` v0.8.1 | **`BSD-2-Clause`** | **Pure Rust** ⭐ | ✅ **Preferred AV1 encoder** — no FFI, no C toolchain, permissive |
| `libaom-sys` v0.17.2 | `BSD-2-Clause` | FFI → libaom (BSD-2 + AOM patent grant) | ✅ but heavy; prefer rav1e/dav1d |
| `aom-sys` v0.3.3 | `MIT` (wrapper) | FFI → libaom | ✅ (verify upstream in THIRD_PARTY) |
| SVT-AV1 | BSD-3-Clause-Clear + AOM patent licence | FFI | ✅ (fast encoder; C dependency) |

**Recommendation: `rav1e` (encode) + `dav1d` (decode).** rav1e being pure Rust and BSD-2 is a strong fit; dav1d is
the fastest AV1 decoder in existence and BSD-2 is unproblematic.

#### H.264 / H.265 — the GPL wall

| Crate / lib | SPDX | Verdict |
|---|---|---|
| `x264` v0.5.0 / libx264 | wrapper `MIT`, **lib GPL-2.0+** (commercial licence available from x264 LLC) | 🔴 **Never in the default build.** §4.4 opt-in only. |
| `x265` v0.1.1 / libx265 | wrapper `MIT`, **lib GPL-2.0+** (commercial from MulticoreWare) | 🔴 Same, and RED on patents too (§2.3) |
| `openh264` / `openh264-sys2` v0.9.8 | **`BSD-2-Clause`** (Cisco) | ⚠️ **Licence-clean, patent-encumbered.** Cisco pays Via LA royalties **only on binaries Cisco itself compiles**. If *we* compile it into *our* binary, **we** need the licence. See §5.2 for the correct pattern. |
| `vpx-sys` v0.1.1 / libvpx | `MIT` wrapper, lib BSD-3-Clause | ✅ licence-wise; VP8 🟢 / VP9 🟡 on patents |
| `ffmpeg-next` / `ffmpeg-sys-next` / `rusty_ffmpeg` | wrappers over LGPL/GPL FFmpeg | 🔴 **Categorically excluded** — using them would defeat the entire premise of the project (clean-room *and* MIT) |

#### Pure-Rust media

| Crate | SPDX | Verdict |
|---|---|---|
| **`symphonia`** v0.6.1 (+ all `symphonia-*` bundles) | **`MPL-2.0`** | ⚠️ **Important.** See §4.2.1. Usable in an MIT binary, but MPL-2.0 is **file-level copyleft**: modifications to Symphonia's *files* must be published under MPL-2.0. It does not infect our files. |
| `mp4parse` v0.17.0 (Mozilla) | **`MPL-2.0`** | ⚠️ Same analysis |
| `av-format` v0.7.1, `av-codec` v0.3.1, `av-data` v0.4.4 (rust-av) | `MIT` | ✅ Ideal licence fit; maturity is the question, not licensing |
| `matroska` v0.30.1 | `MIT/Apache-2.0` | ✅ |
| `image` v0.25.10 | `MIT OR Apache-2.0` | ✅ |
| `png` v0.18.1, `jpeg-decoder` v0.3.2, `gif` v0.14.2, `image-webp` v0.2.4 | `MIT OR Apache-2.0` | ✅ |
| `tiff` v0.11.3 | `MIT` | ✅ |
| `zune-jpeg` v0.5.16 | `MIT OR Apache-2.0 OR Zlib` | ✅ (faster than `jpeg-decoder`) |
| `jxl-oxide` v0.12.6 | `MIT OR Apache-2.0` | ✅ **Pure-Rust JPEG XL** — excellent find |
| `ravif` v0.13.0 | `BSD-3-Clause` | ✅ |
| `claxon` v0.4.3 (FLAC) | **`Apache-2.0`** | ✅ (Apache-only, not dual — fine for us, but note it if we ever needed MIT-only) |
| `lewton` v0.10.2 (Vorbis) | `MIT OR Apache-2.0` | ✅ |
| `puremp3` v0.1.0 | `MIT OR CC0-1.0` | ✅ but immature |
| `minimp3` v0.6.1 | `MIT` wrapper → minimp3 (CC0) | ✅ |
| `symphonia-bundle-mp3` v0.6.1 | `MPL-2.0` | ⚠️ as above |
| `opus` v0.3.1 / `magnum-opus` v0.3.2 | `MIT/Apache-2.0` wrapper → **libopus BSD-3-Clause** | ✅ |
| `audiopus` v0.3.0-rc | `ISC` → libopus BSD-3 | ✅ |
| **Opus, pure Rust** | — | ❌ **Gap.** No production-grade pure-Rust Opus encoder exists. Plan on `libopus` FFI (BSD-3, unproblematic) or budget for writing one. |
| ALAC | Apple reference is **Apache-2.0** | ✅ (express patent grant) |

#### Compression / system

| Crate | SPDX | Verdict |
|---|---|---|
| `flate2` v1.1.9 | `MIT OR Apache-2.0` | ✅ (use `miniz_oxide` backend → pure Rust, no zlib FFI) |
| `miniz_oxide` v0.9.1 | `MIT OR Zlib OR Apache-2.0` | ✅ |
| `bzip2` v0.6.1 | `MIT OR Apache-2.0` (now a **pure-Rust** rewrite by Trifecta Tech) | ✅ |
| `xz2` v0.1.7 | `MIT/Apache-2.0` → liblzma. **XZ Utils relicensed from public-domain to `0BSD` in 2024** | ✅ ⚠️ *Also review the 2024 xz-utils supply-chain backdoor (CVE-2024-3094) — a security, not licence, concern; prefer `lzma-rs` where feasible* |
| `lzma-rs` v0.3.0 | `MIT`, pure Rust | ✅ |
| `zstd` v0.13.3 | `MIT` wrapper → **libzstd, dual `BSD-3-Clause OR GPL-2.0`** — we take the BSD-3 option | ✅ (must record the dual-licence election in `THIRD_PARTY.md`) |
| `brotli` v8.0.4 | `BSD-3-Clause AND MIT` | ✅ |

#### TLS / crypto / network

| Crate | SPDX | Verdict |
|---|---|---|
| **`rustls`** v0.23.43 | `Apache-2.0 OR ISC OR MIT` | ✅ **Recommended default.** Pure Rust, no C toolchain, no OpenSSL cross-compilation misery. |
| `ring` v0.17.14 | **`Apache-2.0 AND ISC`** — note **`AND`, not `OR`**: both apply, and the crate contains BoringSSL/OpenSSL-derived code with its own headers | ✅ but **flag it in `deny.toml` explicitly**; this is the classic cargo-deny false-alarm and needs a documented exception |
| `aws-lc-rs` v1.18.0 | `ISC AND (Apache-2.0 OR ISC)` | ✅ (alternative rustls backend; needs a C toolchain) |
| `openssl` v0.10.81 | `Apache-2.0` wrapper → OpenSSL 3.x is **Apache-2.0**; OpenSSL ≤1.1.1 was the old dual OpenSSL/SSLeay licence, **GPLv2-incompatible** (FFmpeg's `configure:7531` still encodes this) | ⚠️ **Optional feature only.** Apache-2.0 is fine for us, but forcing an OpenSSL dependency hurts users who redistribute under GPLv2. |
| `hyper`, `reqwest`, `quinn` | MIT / MIT-Apache | ✅ |
| SRT (`libsrt`) | **MPL-2.0** | ⚠️ file-level copyleft; acceptable as an optional protocol feature |
| librist | **BSD-2-Clause** | ✅ |

#### Playback (the `ffplay` replacement)

| Option | SPDX | Verdict |
|---|---|---|
| `sdl2` v0.38.0 → **SDL2/SDL3 is `Zlib`** since 2.0 | `MIT` wrapper, `Zlib` lib | ✅ **Zlib licence is fully permissive** — SDL is not a licence problem. It *is* a C dependency and a big one. |
| `winit` v0.30.13 | **`Apache-2.0`** | ✅ |
| `wgpu` v29.0.4 | `MIT OR Apache-2.0` | ✅ |
| `softbuffer` v0.4.8 / `pixels` v0.17.2 | `MIT OR Apache-2.0` / `MIT` | ✅ |
| `cpal` v0.18.2 | **`Apache-2.0`** | ✅ audio output |
| `rodio` v0.22.2 | `MIT OR Apache-2.0` | ✅ |

**Recommendation: `winit` + `wgpu` + `cpal`.** All-Rust, no C toolchain, cross-platform, permissive. SDL2 is a
perfectly legal fallback but pulls in a C build dependency for no licensing benefit. Note `winit` and `cpal` are
**Apache-2.0 only** — which is one more reason for §4.2's recommendation.

#### Subtitles / text shaping

| Option | SPDX | Verdict |
|---|---|---|
| **libass** | **`ISC`** | ✅ Permissive. The obvious choice for ASS/SSA — but it is C, and it pulls FreeType + FriBidi + HarfBuzz. |
| `libass-sys` v0.1.2 | `ISC` | ✅ |
| **FreeType** | **`FTL` OR `GPL-2.0`** — dual. We take **FTL**. | ⚠️ **The FTL has an attribution condition**: you must credit FreeType in your documentation ("Portions of this software are copyright © *year* The FreeType Project (www.freetype.org). All rights reserved."). This is an **advertising-style clause** — not a copyleft, but a real obligation we must discharge in `--version`/docs/NOTICE. |
| HarfBuzz | **Old MIT** ("MIT-Modern-Variant"-ish; permissive, no advertising clause) | ✅ |
| `rustybuzz` v0.20.1 | **`MIT`**, **pure-Rust HarfBuzz port** | ✅ ⭐ **Preferred** — removes the HarfBuzz C dependency |
| `ttf-parser` v0.25.1 | `MIT OR Apache-2.0`, pure Rust | ✅ |
| `swash` v0.2.10 | `Apache-2.0 OR MIT`, pure Rust | ✅ |
| `cosmic-text` v0.19.0 | `MIT OR Apache-2.0`, pure Rust | ✅ |
| `fontdue` v0.9.4 | `MIT OR Apache-2.0 OR Zlib` | ✅ |
| fontconfig | permissive MIT-style (with a "no advertising without permission" clause) | ⚠️ acceptable, but it is a Linux-only config layer |
| `servo-fontconfig` v0.5.1 | `MIT / Apache-2.0` | ✅ |
| `freetype-sys` v0.23.0 | `MIT` wrapper (**upstream is FTL/GPL2**) | ⚠️ **metadata trap** — see §4.0 |

**Recommendation:** an all-Rust text stack — `rustybuzz` + `ttf-parser` + `swash`/`cosmic-text` — plus our own
ASS/SSA renderer written from the (informal but public) SSA/ASS format documentation. This avoids FreeType's FTL
attribution clause and the whole C font stack. It is **more work** than binding libass; the licence benefit is
modest (libass/ISC is fine) but the build-simplicity benefit is large. **If schedule pressure bites, binding
libass is legally acceptable** — just discharge the FTL attribution.

### 4.2 MIT-only or `MIT OR Apache-2.0`? — **Firm recommendation: `MIT OR Apache-2.0`**

**Recommend: dual-license the project `MIT OR Apache-2.0`, the Rust ecosystem standard.** Change this now, before
there are external contributors, because relicensing later requires every copyright holder's consent.

**What Apache-2.0's patent grant actually does.** Section 3:

> "Each Contributor hereby grants to You a perpetual, worldwide, non-exclusive, no-charge, royalty-free,
> **irrevocable** (except as stated in this section) **patent license** to make, have made, use, offer to sell,
> sell, import, and otherwise transfer the Work, where such license applies **only to those patent claims
> licensable by such Contributor** that are necessarily infringed by their Contribution(s) alone or by combination
> of their Contribution(s) with the Work..."
>
> "**If You institute patent litigation** against any entity ... alleging that the Work or a Contribution
> incorporated within the Work constitutes direct or contributory patent infringement, **then any patent licenses
> granted to You under this License for that Work shall terminate** as of the date such litigation is filed."
> <https://www.apache.org/licenses/LICENSE-2.0>

**What that buys us — and be clear-eyed, it is less than people think:**

| ✅ It DOES | ❌ It does NOT |
|---|---|
| Bind **our contributors**: if a contributor's employer holds a patent reading on their contribution, they cannot later assert it against our users | Grant anything from **third parties**. Dolby, Via LA, Access Advance, Sisvel are not contributors and are wholly unaffected. |
| Give downstream users an **express** patent licence rather than relying on an implied one. MIT grants copyright rights and says **nothing** about patents; whether it carries an implied patent licence is unsettled. | Do anything about **codec-essential patents**, which is 100% of our actual patent exposure (§2) |
| Provide **defensive termination** — a patent aggressor who sues over our work loses their own licence to it. A modest but real deterrent. | Protect us from a contributor who *isn't* the patent holder (e.g. an employee contributing without authority) |
| Include an explicit **NOTICE** and trademark-disclaimer regime (§4, §6) | Substitute for a CLA if we ever need broader assurances |

**So: Apache-2.0's patent grant is genuinely useful and genuinely does not solve our patent problem.**
Say this plainly to anyone who suggests it does.

**Why dual rather than Apache-only:**
1. **Ecosystem norm.** Rust itself is `MIT OR Apache-2.0`; the Rust API Guidelines recommend it
   (<https://rust-lang.github.io/api-guidelines/necessities.html>). Deviating creates friction for every downstream
   consumer.
2. **GPLv2 compatibility.** **Apache-2.0 is incompatible with GPLv2** (the patent-termination and indemnity terms
   are "further restrictions"). FFmpeg's own `LICENSE.md` records exactly this problem for VMAF, mbedTLS and
   OpenCORE. If we were Apache-only, **no GPLv2-only project could use us** — which would exclude a large slice of
   the existing FFmpeg-consuming world. Offering MIT as an alternative preserves GPLv2 compatibility.
3. **Maximum downstream freedom** at zero cost to us — the user picks whichever arm suits them.
4. Several dependencies we want (`winit`, `cpal`, `claxon`) are **Apache-2.0 only**, so we cannot in practice
   promise a pure-MIT dependency closure anyway.

**Cost of the change:** essentially nil today (no external contributors yet). Add `LICENSE-MIT` and
`LICENSE-APACHE`, set `license = "MIT OR Apache-2.0"` in every `Cargo.toml`, add the SPDX header to every file,
and use the standard Rust README boilerplate including the contribution clause:

> Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you,
> as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

#### 4.2.1 MPL-2.0 (symphonia, mp4parse, libsrt) — can we ship it in an MIT binary? **Yes.**

MPL-2.0 is **file-level (weak) copyleft**:
- §3.2 permits distributing the Larger Work (our binary) **under our own terms**, provided the MPL-covered
  **files'** source remains available under MPL-2.0.
- §3.3 explicitly permits combining with other licences and distributing the executable under the other licence.
- The obligation is: **if we modify a Symphonia file, we publish that modified file under MPL-2.0.** Our own new
  files are unaffected. Our binary can be distributed under MIT/Apache-2.0 terms.
- MPL-2.0 also carries a **patent grant** (§2.1(b)) and defensive termination (§5.2), similar in spirit to Apache.

**Practical policy:** MPL-2.0 dependencies are **allowed** but flagged in `deny.toml` as `warn`, must be recorded
in `THIRD_PARTY.md`, and **must be consumed unmodified** wherever possible (upstream patches rather than vendoring
forks). We must publish the corresponding source for any MPL file we do modify. If Symphonia becomes a core
dependency we should also consider whether we would rather own that code outright.

> ⚠️ **Strategic note:** Symphonia is MPL-2.0 and is the most complete pure-Rust demux/decode stack in existence.
> Depending on it heavily is legally fine, but it means a large fraction of `vaco`'s actual functionality is
> someone else's weak-copyleft code. That is a **product/architecture** decision as much as a legal one — decide it
> deliberately, not by accident.

### 4.3 CI enforcement

Four tools, four different jobs. All four in CI, all four blocking.

| Tool | Job | Blocking? |
|---|---|---|
| **`cargo-deny`** | Licence allow/deny over the whole dependency graph, plus advisories, bans, duplicate-version and source checks | ✅ yes |
| **`cargo-about`** | Generates the shipped `THIRD_PARTY_LICENSES.html` / `NOTICE` attribution file. **This is a legal obligation**, not a nicety — MIT, BSD, ISC, Apache and FTL all require attribution in redistributed binaries | ✅ yes (fails if a licence text is missing) |
| **REUSE / `reuse lint`** | Per-file SPDX headers; proves per-file provenance (§1.6.3d) | ✅ yes |
| **Custom `xtask licence-audit`** | The §4.0 gap: asserts every `*-sys` / `links` / `build.rs`-compiling crate has an entry in `THIRD_PARTY.md` with the **upstream C library's** licence | ✅ yes |

#### `deny.toml` — concrete starting configuration

```toml
# deny.toml — vaco
# Run: cargo deny check
[graph]
all-features = false          # audit the DEFAULT distributable build
targets = [
  { triple = "x86_64-unknown-linux-gnu" },
  { triple = "aarch64-unknown-linux-gnu" },
  { triple = "x86_64-pc-windows-msvc" },
  { triple = "aarch64-apple-darwin" },
]

[licenses]
version = 2
# Anything not on this list fails the build.
allow = [
  "MIT",
  "Apache-2.0",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "BSD-3-Clause-Clear",     # SVT-AV1
  "ISC",                    # libass, rustls, audiopus
  "Zlib",                   # SDL, miniz_oxide, fontdue
  "0BSD",                   # xz-utils (post-2024)
  "CC0-1.0",                # minimp3, some test data
  "Unicode-3.0",            # unicode-ident et al (successor to Unicode-DFS-2016)
  "Unlicense",
  "BSL-1.0",
  "MPL-2.0",                # symphonia, mp4parse, libsrt — see 4.2.1, must stay unmodified
  "OpenSSL",                # ONLY reachable via the opt-in `tls-openssl` feature
]
confidence-threshold = 0.93
# Crates whose licence expression cargo-deny cannot resolve cleanly.
[[licenses.clarify]]
crate = "ring"
# `Apache-2.0 AND ISC` plus vendored BoringSSL-derived files. Reviewed 2026-08-21.
expression = "Apache-2.0 AND ISC"
license-files = [{ path = "LICENSE", hash = 0xbd0eed23 }]

[[licenses.exceptions]]
crate = "unicode-ident"
allow = ["Unicode-3.0"]

[bans]
multiple-versions = "warn"
wildcards = "deny"
# Nothing GPL/AGPL/LGPL may EVER enter the default graph.
deny = [
  { crate = "x264",         reason = "links GPL libx264 — opt-in `gpl` build only" },
  { crate = "x264-sys",     reason = "links GPL libx264" },
  { crate = "x265",         reason = "links GPL libx265 — opt-in `gpl` build only" },
  { crate = "x265-sys",     reason = "links GPL libx265" },
  { crate = "ffmpeg-sys-next", reason = "LGPL/GPL FFmpeg — defeats the project premise" },
  { crate = "ffmpeg-next",  reason = "ditto" },
  { crate = "rusty_ffmpeg", reason = "ditto" },
  { crate = "libfdk-aac-sys", reason = "FDK-AAC licence is not FOSS-redistributable; AAC is patent-RED anyway" },
]

[advisories]
version = 2
yanked = "deny"
ignore = []

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

**Explicit DENY list (never in any build we distribute):**
`GPL-1.0`, `GPL-2.0`, `GPL-3.0` (and `-only`/`-or-later` variants), `AGPL-*`, `LGPL-*` (static linking is Rust's
default and makes LGPL a practical trap — see below), `SSPL-1.0`, `BUSL-1.1`, `Commons-Clause`, `CC-BY-NC-*`,
`JSON` ("shall be used for Good, not Evil" — not OSI-approved and a genuine compliance hazard), anything
`NONE`/`NOASSERTION`, and Fraunhofer FDK-AAC's licence.

**On LGPL specifically:** the LGPL contemplates that users can relink your binary against a modified version of the
library. **Rust statically links by default and has no stable ABI**, so LGPL §4(d)(0)/(1) compliance means either
shipping our object files for relinking or shipping a dynamic-library build. Neither is something we want to
promise for a static Rust binary. **Policy: no LGPL crates or LGPL C libraries in the default distributable
build.** (This also means, notably, no FFmpeg — which is the point.)

#### CI wiring

```yaml
# .github/workflows/licence.yml
name: licence
on: [push, pull_request]
jobs:
  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
        with: { command: check }
      # Also audit the opt-in feature graphs so we know exactly what they pull in
      - run: cargo deny --all-features check licenses || true   # report-only
  attribution:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo install cargo-about --locked
      - run: cargo about generate about.hbs -o THIRD_PARTY_LICENSES.html --fail
      - run: git diff --exit-code THIRD_PARTY_LICENSES.html   # must be committed & current
  reuse:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: fsfe/reuse-action@v5
  sys-crate-audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo xtask licence-audit    # §4.0 — the metadata trap
  provenance:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo xtask provenance-check # §1.6.3 — DCO + Vaco-Provenance trailers
      - run: cargo xtask similarity-scan  # §1.6.4 — vs a local FFmpeg checkout
```

**Ship with every binary release:** `THIRD_PARTY_LICENSES.html`, an SPDX SBOM (`cargo sbom` / `syft`), and a
`NOTICE` file carrying the FreeType FTL credit (if applicable), the IJG credit (if we ever use libjpeg-derived
code), and our trademark disclaimer (§3.4.5).

### 4.4 The "optional GPL feature flag" model — does it work?

**Short answer: yes, if and only if the GPL code never enters a binary we distribute.** The mechanics matter and
people get them wrong.

#### What is and is not allowed

| Scenario | GPL status of our binary | OK? |
|---|---|---|
| We ship `vaco` compiled with `--features gpl` (statically linking libx264) | **The distributed binary is GPL-2.0+.** Our MIT/Apache source is GPL-compatible so this is *lawful* — but the binary we hand out is now GPL and we must offer corresponding source for the whole work. | ⚠️ Lawful but **not our MIT deliverable**. Do not do this for the default release. |
| A *user* builds `vaco --features gpl` themselves | The user's binary is GPL. **The user is the distributor**, and if they don't redistribute, GPL obligations never trigger at all (GPL restricts distribution, not use). | ✅ **Yes — this is the model.** |
| We ship an MIT binary that `dlopen()`s a GPL plugin the user installed separately | Contested. FSF says a plugin with intimate coupling forms a combined work; the counter-view under *Altai* is that the boundary matters. **Genuinely unsettled — no US appellate decision.** | ⚠️ **Do not rely on this as our primary architecture.** |
| We ship an MIT binary that `fork()`/`exec()`s a separate GPL process, communicating over pipes/CLI | Strongest position. Separate programs at arm's length communicating over a documented protocol are an "aggregate", not a combined work — this is the mechanism the FSF itself has always accepted. | ✅ **Yes — preferred escape hatch.** |
| We ship an MIT binary + separately ship a GPL binary in the same tarball | Mere aggregation on a distribution medium. Each keeps its own licence. | ✅ Yes, but confusing for users; prefer separate packages |

#### Recommended mechanics

**1. Cargo feature flags are necessary but not sufficient.** A feature flag alone is fine for *building*, but
`cargo` features are additive and easy to enable accidentally in a workspace. Belt and braces:

```
vaco-workspace/
├── vaco/                  # the binary. Default features = permissive only.
├── vaco-core/             # MIT OR Apache-2.0. No GPL deps, ever.
├── vaco-codecs/           # MIT OR Apache-2.0 permissive codecs
├── contrib/               # SEPARATE workspace, SEPARATE repo, NOT published to crates.io
│   ├── vaco-x264/         # GPL-2.0+ crate. Its own deny.toml. Its own release process.
│   └── vaco-x265/         # GPL-2.0+
└── deny.toml              # default graph must contain zero GPL — CI-enforced
```

- Put GPL integrations in a **separate repository and separate crates**, each licensed **GPL-2.0-or-later**
  (a permissive crate that links GPL code is misleadingly labelled; label it honestly).
- **Never publish the GPL crates to crates.io as part of our namespace's default resolution path**, and never
  list them as optional dependencies of `vaco` itself — require an explicit `[patch]` or path dependency, so
  enabling GPL is a deliberate act of editing a manifest, not flipping a flag.
- **CI gate:** `cargo deny check` runs with `all-features = false` on the release graph and **fails** on any
  GPL/LGPL crate (see the `[bans].deny` list in §4.3). Add a second job that runs
  `cargo tree --all-features` and asserts the deny-list crates appear **nowhere** in the release build plan.

**2. Prefer the process boundary over the link boundary.** For x264/x265, the cleanest architecture is:

```
vaco --c:v h264 --encoder-backend external:x264
   → spawns `x264 --demuxer raw ... -o -` and pipes frames
```

This is unambiguously mere aggregation, works with the user's distro-installed `x264` binary (already GPL and
already legitimately theirs), requires **no GPL code in our tree at all**, and sidesteps the whole question.
The performance cost of a pipe is small relative to encoding cost. **Strongly recommended.**

**3. The same pattern solves the patent problem, not just the licence problem.** A user who installs
`x264` from their distro obtains a codec their distro already decided to ship. We never distribute an H.264
encoder, so we never create a licensable unit (§2.1). **The licence escape hatch and the patent escape hatch are
the same mechanism** — this is the key architectural insight of this document.

**4. Does it keep our distributed binary MIT-clean? Yes**, provided:
- ✅ the release build is produced from the default feature set, in CI, with `cargo deny` passing;
- ✅ the release artefacts are byte-reproducible from a tagged commit (publish the build recipe + SBOM);
- ✅ no GPL crate appears anywhere in the release `Cargo.lock`;
- ✅ we never publish a "convenience" full build ourselves. **The moment we publish an `ffmpeg-full`-style binary
  with x264/x265/fdk-aac, we have (a) a GPL binary, (b) an AVC/HEVC patent unit, and (c) an unredistributable
  fdk-aac artefact. That is the one mistake that undoes everything in this document.** Make it structurally
  impossible: the release workflow must have no code path that can produce it.

---

## 5. FINAL RECOMMENDATION

### 5.1 (a) Default distributable build — "vaco"

Licence: **MIT OR Apache-2.0**. Patent posture: expired, royalty-free, or hardware-delegated only.
This is what we compile in CI and publish on the releases page for every platform.

**Video decode:** H.261 · H.263 · MPEG-1 · MPEG-2 (H.262) · MPEG-4 Part 2 · **AV1** (dav1d) · VP8 · VP9† ·
Theora · CineForm · MJPEG · FFV1‡ · HuffYUV · Lagarith · QuickTime RLE/Animation · Cinepak · MS Video 1 ·
MS Screen 1/2 · TSCC · ZMBV · Dirac/VC-2 · raw/uncompressed · **ProRes decode**† · DNxHD/VC-3 decode†

**Video encode:** **AV1** (rav1e) · MPEG-1/2/4-Part-2 · MJPEG · FFV1 · HuffYUV · VP8/VP9† · ZMBV · raw

**Audio decode:** **Opus** · Vorbis · FLAC · ALAC · **MP3** · MP2 · **AC-3** · **E-AC-3**† · PCM (all) ·
ADPCM (all) · G.711/G.722/G.726/G.729 · Speex · WavPack · TTA · Shorten · ALS · Musepack · Wavesynth ·
game audio (Bink, Smacker, ADX, etc. — subject to T2 clean-room, §1.7)

**Audio encode:** **Opus** (libopus) · Vorbis · FLAC · ALAC · **MP3** (own encoder) · MP2 · PCM · ADPCM · AC-3 · G.711/G.722

**Images:** JPEG · PNG · GIF · BMP · TIFF · WebP · **JPEG XL** (jxl-oxide) · JPEG 2000 · AVIF† · PPM/PGM/PNM · TGA · QOI · DDS · EXR

**Containers:** MP4/ISOBMFF/MOV · Matroska/WebM · Ogg · WAV/RIFF · AVI · FLAC · AIFF · CAF · MXF ·
MPEG-PS · MPEG-TS† · ASF/WMV (demux only)† · HLS · DASH · raw ES

**Protocols:** file · pipe · http/https (rustls) · tcp/udp · rtp/rtsp · srt (MPL) · rist · s3-style via feature

**Filters, scaling, colour, resampling, muxing, the CLI, ffprobe, ffplay** — all our own code, all MIT/Apache.

**Hardware acceleration for encumbered codecs** (§2.4) — VideoToolbox, VA-API, D3D11VA/MF, NVDEC/NVENC, AMF,
MediaCodec. **This is how users get H.264/HEVC in the default build**: we ship the plumbing, their hardware ships
the licensed codec. No software AVC/HEVC implementation is in our binary.

† *Conditions:* VP9/AV1/AVIF — AMBER per §2.3 (Sisvel/Dolby exposure); ship, but track *Dolby v. Snap*.
E-AC-3 — ship **only after** the 2026-01-30 expiry is independently verified (§5.4 Q3).
ProRes/DNxHD — **decode only**, never encode.
MPEG-TS — verify MPEG-2 Systems programme status before shipping a **muxer**.
ASF — demux only.
‡ FFV1 is an FFmpeg-originated format (now IETF-standardised, RFC 9043) — implement from the RFC, not the source.

### 5.2 (b) Opt-in, build-it-yourself — "vaco-contrib"

Separate repo, separate crates, correctly-licensed, **never in a binary we publish**. Documented, supported,
CI-tested — just not distributed as a binary by us.

| Component | Why it's here | Mechanism |
|---|---|---|
| **libx264 / libx265** | GPL **and** patent-encumbered | **Preferred: `exec` the user's system `x264`/`x265` binary** (§4.4.2) — zero GPL in our tree. Fallback: a GPL-licensed `vaco-x264` crate the user opts into. |
| **H.264 software encode/decode** | AVC patents live until ~2027–28 | Three user-selectable paths: (1) **hardware** (in default build); (2) **system codec** (Media Foundation / VideoToolbox); (3) **Cisco's pre-built OpenH264 binary downloaded at runtime**, the Firefox pattern — Cisco's royalties cover Cisco's binaries. **We must not compile OpenH264 into our binary.** |
| **HEVC / VVC software** | Multi-pool RED (§2.3) | Hardware / system codec only. No software implementation shipped, ever. |
| **AAC (LC/HE/xHE) encode & decode** | Via LA pool is **active** | Painful but necessary. Options: system codec (AudioToolbox/MediaFoundation), or an opt-in build against a decoder the user licenses. Note the AAC pool charges on **encoder/decoder units, not bitstreams** — so *remuxing* AAC in an MP4 without decoding is fine and stays in the default build. |
| **fdk-aac** | Licence is not FOSS-redistributable (FFmpeg marks it `nonfree`/unredistributable) | Build-from-source instructions only |
| **DTS, AC-4, Dolby Vision, EVS, AMR-WB** | Active direct licensing | Documented as unsupported; system codec where the OS provides it |
| **AVS2/AVS3** | Unverifiable Chinese pool posture | Opt-in decode if a user asks |
| **OpenSSL TLS backend** | Apache-2.0 is fine for us but hurts GPLv2 redistributors | Cargo feature; default is rustls |
| **libass binding** | ISC is fine; it's here only because we prefer the pure-Rust stack | Feature flag |

**Documentation duty:** every entry needs a page explaining *why* it isn't in the default build, distinguishing
**licence** reasons from **patent** reasons, so users can make their own informed decision. This is the single
most valuable piece of documentation the project can write, and it is the thing FFmpeg has never done well.

### 5.3 (c) Never

| Component | Reason |
|---|---|
| **Anything linking FFmpeg/libav** (`ffmpeg-sys-next`, `ffmpeg-next`, `rusty_ffmpeg`) | Destroys both the clean-room premise and the MIT deliverable |
| **fdk-aac in a distributed binary** | Unredistributable |
| **A software HEVC/VVC encoder or decoder in a binary we distribute** | 🔴 Multi-pool, active, injunction-seeking holders. No mitigation exists short of paying three pools. |
| **DRM: Widevine, PlayReady, FairPlay, AACS/BD+, CSS** | Licensing + **DMCA § 1201 anti-circumvention** (and EU Copyright Directive Art. 6). A whole separate legal regime. |
| **Any name, logo or version string that claims to be FFmpeg** | §3 |
| **Copying any FFmpeg source, table, comment, option-table text, `.xsd`, or FATE reference set** | §1 |
| **AI-assisted C→Rust translation of FFmpeg** | §1.6.4 |
| **Shipping a "full"/"everything" convenience binary** | §4.4.4 — the one mistake that undoes the whole strategy |

### 5.4 Top 5 questions that genuinely require a lawyer

**Q1 — Binary distribution and contributory infringement, for the AMBER codecs.**
If we distribute a binary containing AV1/VP9/AVIF (AOM-covered but with live non-member assertions — *Dolby v.
Snap*, D. Del. 1:26-cv-00317, March 2026) and our users are sued, what is our exposure for **induced or
contributory infringement** under 35 U.S.C. § 271(b)/(c)? Does shipping documentation that tells users
"use `-c:v av1`" strengthen an inducement claim? *This determines whether §5.1 is viable at all.*

**Q2 — Entity structure and jurisdiction.**
Where should the project and any commercial entity sit, and what does that change? VideoLAN's French non-profit
posture is a deliberate structural choice, not an accident. Given the UPC's pan-European injunctions since 2023,
is "be European" still the mitigation people assume? What does a US LLC vs. a foundation vs. an offshore entity
change about reachability, damages exposure, and insurance availability? **Ask this before incorporating, not
after.**

**Q3 — Verified patent-landscape search for the codecs we intend to ship.**
Several load-bearing expiry claims in §2.3 rest on single secondary sources: **E-AC-3 (US 7,516,064, 2026-01-30)**,
**MPEG-4 Part 2 (BR, 2026-07-19)**, MPEG-2 Systems (as distinct from MPEG-2 Video), G.723.1, G.726, AAC-LC's
remaining tail, and the exact AVC tail. Commission a professional **freedom-to-operate / patent-landscape search**
covering the §5.1 codec list in US, EU, UK, JP, KR, CN, BR. This is the highest-value legal spend available and it
converts most of the AMBER entries to a defensible position either way.

**Q4 — Copyright: the clean-room protocol and the constant-table position.**
Review §1.6's workflow and give a written opinion on (i) whether spec-first + attestation (T1) is adequate or
whether we must run the two-team protocol more broadly; (ii) the Tier-1 spec-dictated-constant-table position
(§1.5b), which touches every codec; (iii) the CLI-surface and ffprobe-schema positions (§1.5a, §1.5e); and
(iv) whether the AI-assistant controls in §1.6.4 are sufficient. Ask for something we can **put in front of an
acquirer's diligence team**, because that is when this gets tested in practice.

**Q5 — Trademark clearance for `vaco` and for the compatibility shim.**
(i) Clear `vaco` itself — full knock-out search in the relevant classes and jurisdictions before we build a brand
on it. (ii) Is the opt-in `ffmpeg`/`ffprobe`/`ffplay` shim (§3.4.2) acceptable, and is the
`ffmpeg-cli compatibility: 7.1` version string acceptable? (iii) Confirm the registration status of "FFmpeg" via
USPTO/EUIPO/WIPO — my research found only FFmpeg's **unilateral assertion** that it is Fabrice Bellard's
trademark, with no registration confirmed. (iv) Confirm the descriptive-use position on codec identifiers like
`ac3`, `eac3`, `dts`, `prores` where the patents have expired but the **marks and certification programmes have
not**.

---

## 6. Immediate action list

| # | Action | Owner | When |
|---|---|---|---|
| 1 | Change project licence to `MIT OR Apache-2.0`; add `LICENSE-MIT`, `LICENSE-APACHE`, SPDX headers, README boilerplate | eng lead | **Before the first external contributor** |
| 2 | Adopt the DCO + `Vaco-Provenance` trailer; add the PR template checklist (§1.6.3b) | eng lead | Week 1 |
| 3 | Land `deny.toml` (§4.3) + the five CI jobs; make them blocking | eng | Week 1 |
| 4 | Create `provenance/`, `THIRD_PARTY.md`, `NOTICE` | eng | Week 1 |
| 5 | Write `docs/why-some-codecs-are-not-included.md` (§5.2) | eng lead | Week 2 |
| 6 | Build the CI similarity scan vs a local FFmpeg checkout (§1.6.4) | eng | Month 1 |
| 7 | Join **OIN** (free) | eng lead | Month 1 |
| 8 | Confirm the **AOM Patent License 1.0** reciprocity/reproduction condition is satisfied for AV1 (we must reproduce the licence with our distribution) | eng lead | Before first AV1 release |
| 9 | Engage counsel on Q1–Q5 (§5.4) | founder | **Before the first binary release** |
| 10 | Commission the FTO search (Q3) | founder | Before commercial launch |
| 11 | Re-review this document when *Dolby v. Snap* resolves, and when the Access Advance/Via LA HEVC consolidation completes (targeted end-2026) | eng lead | Standing |

---

## Appendix A — Confidence levels

| Claim | Confidence |
|---|---|
| Rust rewrite does not change patent exposure (§2.6) | **Very high** — black-letter law |
| Patents are the main reason FFmpeg ships no binaries (§2.1) | **High** — FFmpeg's own legal page |
| MP3, AC-3, MPEG-2 Video, JPEG, G.729 fully expired | **High** — multiple sources, well-documented dates |
| HEVC/VVC/AAC/DTS/EVS actively licensed and RED | **High** — pools publicly active in 2026 |
| *Dolby v. Snap* facts and AV1 downgrade to AMBER (§2.3) | **High** — Access Advance's own press release + multiple outlets |
| Access Advance acquired Via LA's HEVC/VVC pools 2025-12-15 | **High** — multiple sources |
| **E-AC-3 expired 2026-01-30** | **LOW** — single hedged secondary source reading Google Patents. **Verify before relying on it.** |
| **MPEG-4 Part 2 last (BR) patent expired 2026-07-19** | **LOW** — two closely-related secondary sources |
| MPEG-2 Systems (TS) programme status | **Low** — not separately verified |
| AVS2/AVS3 rates and enforcement | **Very low** — English-language sources inadequate |
| ProRes patent position | **Low** — no public essential-patent list exists |
| DNxHD/VC-3 Avid licence requirement | **Low** — community sources only |
| FFmpeg trademark **registration** status | **Unverified** — only a unilateral assertion found; no database search run |
| ITU/JVET conformance bitstream licence terms | **Unverified** — no explicit licence located |
| Crate SPDX identifiers in §4.1 | **High** — queried from the crates.io API 2026-08-21 |
| Upstream C library licences behind `*-sys` crates | **Medium** — from knowledge + spot checks, **not** systematically verified. See §4.0. |

## Appendix B — Principal sources

**FFmpeg:** <https://www.ffmpeg.org/legal.html> · `~/repos/FFmpeg/LICENSE.md` · `~/repos/FFmpeg/configure` ·
`~/repos/FFmpeg/doc/faq.texi` · `~/repos/FFmpeg/doc/ffprobe.xsd`

**Cases:** *Google LLC v. Oracle America*, 593 U.S. 1 (2021) <https://www.supremecourt.gov/opinions/20pdf/18-956_d18f.pdf> ·
*Oracle v. Google*, 750 F.3d 1339 (Fed. Cir. 2014) <https://law.justia.com/cases/federal/appellate-courts/cafc/13-1021/13-1021-2014-05-09.html> ·
*Computer Associates v. Altai*, 982 F.2d 693 (2d Cir. 1992) <https://law.justia.com/cases/federal/appellate-courts/F2/982/693/137252/> ·
*Sega v. Accolade*, 977 F.2d 1510 (9th Cir. 1992) <https://law.justia.com/cases/federal/appellate-courts/F2/977/1510/305345/> ·
*Sony v. Connectix*, 203 F.3d 596 (9th Cir. 2000) <https://law.justia.com/cases/federal/appellate-courts/F3/203/596/474793/> ·
*Lotus v. Borland*, 49 F.3d 807 (1st Cir. 1995) <https://www.bitlaw.com/source/cases/copyright/Lotus.html> ·
*Feist v. Rural Telephone*, 499 U.S. 340 (1991) <https://supreme.justia.com/cases/federal/us/499/340/> ·
*New Kids on the Block v. News America*, 971 F.2d 302 (9th Cir. 1992) ·
*Toyota v. Tabari*, 610 F.3d 1171 (9th Cir. 2010) <https://cdn.ca9.uscourts.gov/datastore/opinions/2010/07/08/07-55344.pdf> ·
*SAS Institute v. World Programming*, C-406/10 (CJEU 2012) <https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=celex:62010CJ0406> ·
Directive 2009/24/EC <https://eur-lex.europa.eu/eli/dir/2009/24/oj/eng>

**Pools & licences:** Via LA AVC <https://www.via-la.com/licensing-programs/avc-h-264/> ·
Via LA MPEG-2 <https://www.via-la.com/licensing-programs/mpeg-2/> ·
Via LA AAC <https://www.via-la.com/licensing-programs/aac/> and FAQ <https://via-la.com/licensing-2/aac/aac-faqs> ·
Via LA Voice Codec (EVS) <https://www.via-la.com/licensing-programs/voice-codec/> ·
Access Advance HEVC <https://accessadvance.com/licensing-programs/hevc-advance/> ·
Access Advance 2026 rate deferral <https://accessadvance.com/2026/01/27/access-advance-extends-hevc-advance-rate-increase-deadline/> ·
Access Advance acquires Via LA HEVC/VVC pools <https://ipfray.com/breaking-access-advance-acquires-via-licensing-alliances-hevc-vvc-patent-pools/> ·
Sisvel VP9/AV1 <https://www.sisvel.com/licensing-programmes/audio-and-video-coding-decoding/video-coding-platform-av1/> ·
AOM Patent License 1.0 <https://aomedia.org/license/patent-license/> ·
OIN Linux System <https://openinventionnetwork.com/linux-system/> ·
Apache-2.0 <https://www.apache.org/licenses/LICENSE-2.0> ·
Google VP8 cross-licence <https://www.webmproject.org/cross-license/vp8/agreement/> ·
Opus licence <https://opus-codec.org/license/> ·
JPEG XL PATENTS <https://github.com/libjxl/libjxl/blob/main/PATENTS> ·
Nokia HEIF licence <https://github.com/nokiatech/heif/wiki/VI.-License>

**2026 litigation:** <https://accessadvance.com/2026/03/24/access-advance-licensor-sues-snap-inc-for-av1-and-hevc-patent-infringement/> ·
<https://ipfray.com/dolby-sues-snapchat-over-av1-and-hevc-patent-infringement-in-u-s-and-brazil-access-advance-vdp-license-would-resolve-issue/> ·
<https://streaminglearningcenter.com/encoding/when-theres-no-frand-what-dolbys-suit-against-snap-means-for-the-industry.html>

**Expiries:** MPEG-2 <https://www.phoronix.com/scan.php?page=news_item&px=MPEG-2-Last-Patents-Expire> ·
MP3 <https://www.iis.fraunhofer.de/en/ff/amm/consumer-electronics/mp3.html> ·
AC-3 <https://freetoairamerica.wordpress.com/2017/03/20/electronic-frontier-foundation-the-patent-on-dolby-digital-ac-3-has-just-expired/> ·
E-AC-3 <https://www.phoronix.com/news/Dolby-Digital-Plus-E-AC3-2026> ·
G.729 <https://www.mgraves.org/2017/03/its-official-the-patents-on-g-729-have-expired/> ·
MPEG-4 Part 2 <https://xenospectrum.com/en/mpeg4-divx-xvid-patent-expires/>

**Practice:** DCO <https://developercertificate.org/> and <https://wiki.linuxfoundation.org/dco> ·
REUSE <https://reuse.software/spec-3.3/> ·
ReactOS audit <https://www.linux.com/news/reactos-suspends-development-source-code-review/> ·
Cisco OpenH264 <https://blogs.cisco.com/collaboration/ciscos-openh264-now-part-of-firefox> ·
VideoLAN on patents <http://www.videolan.org/press/patents.html> ·
Libav/avconv rename <https://blog.pkh.me/p/13-the-ffmpeg-libav-situation.html> ·
OpenSSL trademark policy <https://openssl-library.org/policies/general/trademarkpolicy/> ·
Debian–Mozilla dispute <https://en.wikipedia.org/wiki/Debian%E2%80%93Mozilla_trademark_dispute> ·
Rust API Guidelines on licensing <https://rust-lang.github.io/api-guidelines/necessities.html>

**Crate licences:** queried from the crates.io API, 2026-08-21.
