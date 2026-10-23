#!/usr/bin/env bash
# Regenerate the QC-preview test fixtures (see README.md here). Deterministic: the reads come from a
# fixed seed, so a rerun writes the same FASTA, SAM and FASTQ (BAM/zip bytes can differ by tool
# version). Needs python3, zip/unzip and pixi (samtools 1.21, FastQC 0.12.1 and MultiQC 1.35
# come from bioconda).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 - "$work" <<'EOF'
import random
import sys

work = sys.argv[1]
rng = random.Random(8)
# Two contigs: chrA has no reads, chrB has reads from ~9 kb, so "the first contig with data" is
# chrB and its first read does not start in the first 16 kb linear-index window's first base.
contigs = {"chrA": 4000, "chrB": 12000}
seqs = {name: "".join(rng.choice("ACGT") for _ in range(n)) for name, n in contigs.items()}
with open(f"{work}/tiny.fa", "w") as fa:
    for name, seq in seqs.items():
        fa.write(f">{name}\n")
        for i in range(0, len(seq), 60):
            fa.write(seq[i : i + 60] + "\n")

# 200 exact 100 bp matches on chrB starting in 9001..9500 (1-based), as a SAM.
with open(f"{work}/tiny.sam", "w") as sam:
    sam.write("@HD\tVN:1.6\tSO:unsorted\n")
    for name, n in contigs.items():
        sam.write(f"@SQ\tSN:{name}\tLN:{n}\n")
    for i in range(200):
        pos = rng.randint(9001, 9500)
        seq = seqs["chrB"][pos - 1 : pos + 99]
        sam.write(f"r{i:03d}\t0\tchrB\t{pos}\t60\t100M\t*\t0\t0\t{seq}\t{'I' * 100}\n")

# 2,000 reads of 100 bp for FastQC, quality falling along the read (so some modules warn or fail).
with open(f"{work}/S1_R1.fastq", "w") as fq:
    chrb = seqs["chrB"]
    for i in range(2000):
        pos = rng.randint(0, len(chrb) - 100)
        seq = chrb[pos : pos + 100]
        quals = "".join(chr(33 + max(2, min(40, int(38 - j * 0.3 + rng.gauss(0, 4))))) for j in range(100))
        fq.write(f"@read{i}\n{seq}\n+\n{quals}\n")
EOF

samtools() { pixi exec -c conda-forge -c bioconda --spec "samtools==1.21" samtools "$@"; }

# ── igv: a sorted, indexed BAM against a two-contig FASTA ──────────────────────────────────────
samtools faidx "$work/tiny.fa"
samtools sort --no-PG -o "$work/tiny.bam" "$work/tiny.sam"
samtools index "$work/tiny.bam"
samtools index -c -o "$work/tiny.bam.csi" "$work/tiny.bam"
# The CRAM's own index only (the locus is read from it and the CRAM's header, which the tests
# build); the CRAM itself is not kept.
samtools view --no-PG -C -T "$work/tiny.fa" -o "$work/tiny.cram" "$work/tiny.bam"
samtools index "$work/tiny.cram"
cp "$work/tiny.fa" "$work/tiny.fa.fai" "$work/tiny.bam" "$work/tiny.bam.bai" "$work/tiny.bam.csi" \
  "$work/tiny.cram.crai" "$here/igv/"

# ── fastqc: FastQC 0.12.1's own output, with its images and HTML dropped from the zip ──────────
pixi exec -c conda-forge -c bioconda --spec "fastqc==0.12.1" \
  fastqc --quiet --outdir "$work" "$work/S1_R1.fastq"
(cd "$work" && unzip -q S1_R1_fastqc.zip S1_R1_fastqc/fastqc_data.txt S1_R1_fastqc/summary.txt \
  && rm S1_R1_fastqc.zip && zip -q -X S1_R1_fastqc.zip S1_R1_fastqc/fastqc_data.txt S1_R1_fastqc/summary.txt)
cp "$work/S1_R1_fastqc.zip" "$here/fastqc/"
cp "$work/S1_R1_fastqc/fastqc_data.txt" "$here/fastqc/"

# ── multiqc: MultiQC 1.35's FastQC table for three FASTQs (S1's pair and S10's R1) ─────────────
# Gzipped reads, so MultiQC names each by its FASTQ without `.fastq.gz` (`S1_R1`); S10's quality is
# low (some modules fail) and every FASTQ's GC content is uniform (a module warns).
mkdir -p "$work/mqc"
python3 - "$work/mqc" <<'EOF'
import gzip
import random
import sys

work = sys.argv[1]
rng = random.Random(13)


def fastq(name, n, gc, low, high, mixed_quality):
    with gzip.open(f"{work}/{name}.fastq.gz", "wt") as fq:
        for i in range(n):
            seq = "".join(rng.choices("ACGT", weights=[1 - gc, gc, gc, 1 - gc], k=100))
            floor = low if not mixed_quality or i % 3 == 0 else 20
            quals = "".join(chr(33 + rng.randint(floor, high)) for _ in seq)
            fq.write(f"@r{i}\n{seq}\n+\n{quals}\n")


fastq("S1_R1", 1500, 0.5, 2, 40, True)
fastq("S1_R2", 1500, 0.5, 2, 40, True)
fastq("S10_R1", 1000, 0.3, 2, 14, False)
EOF
pixi exec -c conda-forge -c bioconda --spec "fastqc==0.12.1" \
  fastqc --quiet --outdir "$work/mqc" "$work/mqc/S1_R1.fastq.gz" "$work/mqc/S1_R2.fastq.gz" \
  "$work/mqc/S10_R1.fastq.gz"
pixi exec -c conda-forge -c bioconda --spec "multiqc==1.35" \
  multiqc --quiet --force --outdir "$work/mqc/out" "$work/mqc"
cp "$work/mqc/out/multiqc_data/multiqc_fastqc.txt" "$here/multiqc/"

# ── multiqc sections: MultiQC 1.35 running its FastQC module twice, raw and trimmed reads ──────
# MultiQC's documented setup (docs/markdown/reports/customisation.md, `module_order`): one FastQC
# section per anchor, so `multiqc_data/` holds `multiqc_fastqc_fastqc_raw.txt` and
# `multiqc_fastqc_fastqc_trimmed.txt` and no `multiqc_fastqc.txt`. The trimmed reads are the raw
# ones cut to their first 80 bases, under the same file names (so the same MultiQC names appear in
# both sections).
mkdir -p "$work/sections/raw" "$work/sections/trimmed"
python3 - "$work/mqc" "$work/sections/trimmed" <<'EOF2'
import gzip
import sys

src, dst = sys.argv[1], sys.argv[2]
for name in ["S1_R1", "S1_R2", "S10_R1"]:
    with gzip.open(f"{src}/{name}.fastq.gz", "rt") as fin, gzip.open(f"{dst}/{name}.fastq.gz", "wt") as fout:
        lines = fin.read().splitlines()
        for i in range(0, len(lines), 4):
            head, seq, plus, qual = lines[i : i + 4]
            fout.write(f"{head}\n{seq[:80]}\n{plus}\n{qual[:80]}\n")
EOF2
cp "$work/mqc"/*_fastqc.zip "$work/sections/raw/"
pixi exec -c conda-forge -c bioconda --spec "fastqc==0.12.1" \
  fastqc --quiet --outdir "$work/sections/trimmed" "$work/sections/trimmed/S1_R1.fastq.gz" \
  "$work/sections/trimmed/S1_R2.fastq.gz" "$work/sections/trimmed/S10_R1.fastq.gz"
cat > "$work/sections/multiqc_config.yaml" <<'EOF2'
module_order:
  - fastqc:
      name: "FastQC (raw)"
      anchor: "fastqc_raw"
      path_filters:
        - "*/raw/*_fastqc.zip"
  - fastqc:
      name: "FastQC (trimmed)"
      anchor: "fastqc_trimmed"
      path_filters:
        - "*/trimmed/*_fastqc.zip"
EOF2
pixi exec -c conda-forge -c bioconda --spec "multiqc==1.35" \
  multiqc --quiet --force --config "$work/sections/multiqc_config.yaml" \
  --outdir "$work/sections/out" "$work/sections/raw" "$work/sections/trimmed"
mkdir -p "$here/multiqc/sections"
cp "$work/sections/out/multiqc_data/multiqc_fastqc_fastqc_raw.txt" \
  "$work/sections/out/multiqc_data/multiqc_fastqc_fastqc_trimmed.txt" "$here/multiqc/sections/"
