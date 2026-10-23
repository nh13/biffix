# QC-preview test fixtures

Small outputs of the real tools, read by `src/qc/fastqc.rs`, `src/qc/locus.rs`, `src/qc/multiqc.rs`,
`tests/it/qc_previews.rs` and `tests/it/resumed_outputs.rs`.
Every file is written by `make_fixtures.sh` (deterministic: the FASTA, SAM and FASTQ come from a
fixed seed; a rerun with the same tool versions reproduces these bytes):

```sh
bash crates/biffix-backend/tests/fixtures/make_fixtures.sh
```

It needs `python3`, `zip`/`unzip` and `pixi`; samtools, FastQC and MultiQC run from bioconda through
`pixi exec`.

## `igv/` — a tiny indexed BAM against a two-contig FASTA

`tiny.fa` has `chrA` (4,000 bp, no reads) and `chrB` (12,000 bp); `tiny.sam` (not kept) holds 200
exact 100 bp matches on `chrB` starting in 9,001–9,500, so "the first contig with reads" is `chrB`
and the first read starts at 9,001 (locus `chrB:9001-11000`).

```sh
pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools faidx tiny.fa
pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools sort --no-PG -o tiny.bam tiny.sam
pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools index tiny.bam                       # tiny.bam.bai
pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools index -c -o tiny.bam.csi tiny.bam
pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools view --no-PG -C -T tiny.fa -o tiny.cram tiny.bam
pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools index tiny.cram                      # tiny.cram.crai
```

Kept: `tiny.fa`, `tiny.fa.fai`, `tiny.bam`, `tiny.bam.bai`, `tiny.bam.csi`, `tiny.cram.crai` (the
CRAM itself is not kept: its locus is read from its index and its header, which the tests build;
the header reader was checked against samtools-written CRAM 3.0 and 2.1 files).

## `fastqc/` — FastQC 0.12.1's own result for 2,000 reads

`S1_R1.fastq` (not kept) holds 2,000 100 bp reads from `chrB` with quality falling along the read,
so some modules warn or fail.

```sh
pixi exec -c conda-forge -c bioconda --spec "fastqc==0.12.1" fastqc --quiet --outdir . S1_R1.fastq
unzip -q S1_R1_fastqc.zip S1_R1_fastqc/fastqc_data.txt S1_R1_fastqc/summary.txt
rm S1_R1_fastqc.zip
zip -q -X S1_R1_fastqc.zip S1_R1_fastqc/fastqc_data.txt S1_R1_fastqc/summary.txt
```

Kept: `S1_R1_fastqc.zip` (FastQC's zip with its images and HTML dropped, to stay small) and the
`fastqc_data.txt` inside it.

## `multiqc/` — MultiQC 1.35's FastQC table for three FASTQs

`S1_R1`, `S1_R2` and `S10_R1` (`.fastq.gz`, not kept) hold 1,500, 1,500 and 1,000 random 100 bp
reads from a fixed seed; S10's qualities are low, so two of its modules fail, and S1_R1's GC
module warns. FastQC 0.12.1 reads each, then MultiQC 1.35 reads FastQC's output;
`src/qc/multiqc.rs` and `tests/it/qc_previews.rs` read MultiQC's `multiqc_data/multiqc_fastqc.txt` (a
TSV: `Sample`, FastQC's basic statistics, then each module's status), and the UI's mock serves it.

```sh
pixi exec -c conda-forge -c bioconda --spec "fastqc==0.12.1" fastqc --quiet --outdir . S1_R1.fastq.gz S1_R2.fastq.gz S10_R1.fastq.gz
pixi exec -c conda-forge -c bioconda --spec "multiqc==1.35" multiqc --quiet --force --outdir out .
```

Kept: `multiqc_fastqc.txt` only (the report and the rest of `multiqc_data/` are not read here).

### `multiqc/sections/` — MultiQC 1.35 running its FastQC module twice: raw and trimmed reads

The same three FASTQs (raw) and each cut to its first 80 bases (trimmed, under the same file names
in another folder), FastQC 0.12.1 on each, then MultiQC 1.35 with the `module_order` setup MultiQC
documents for FastQC before and after trimming (`docs/markdown/reports/customisation.md`): one
FastQC section per anchor, picked by `path_filters`:

```yaml
module_order:
  - fastqc:
      name: "FastQC (raw)"
      anchor: "fastqc_raw"
      path_filters: ["*/raw/*_fastqc.zip"]
  - fastqc:
      name: "FastQC (trimmed)"
      anchor: "fastqc_trimmed"
      path_filters: ["*/trimmed/*_fastqc.zip"]
```

MultiQC names each section's table `multiqc_fastqc_<anchor>.txt` (`BaseMultiqcModule.write_data_file`
appends the anchor) and writes no `multiqc_fastqc.txt`. Kept: `multiqc_fastqc_fastqc_raw.txt` and
`multiqc_fastqc_fastqc_trimmed.txt`; both hold the names `S1_R1`, `S1_R2` and `S10_R1`.

## `project_context.json` — the per-project context contract

Hand-written, not generated: one project's SSM parameter value (`/biffix/<tenant>/projects/<id>`)
as the deploy writes it. `src/config/projects.rs` parses it and asserts it holds exactly the keys the
backend reads; infra's synth test compares the keys its own JSON emits against the same file, so
the two sides of the contract cannot drift apart.
