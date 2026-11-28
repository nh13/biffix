// Single-job acceptance (dev/smoke/single-job/README.md). Every task checks that it runs natively
// on the expected machine, in the image it names, without the run's capability token in its
// environment and, nested in Apptainer, in its container with the run's shared /tmp. FIRST reads a
// synthetic S3 input; `validation_case` makes it retry once, fail for good, or wait to be cancelled.
// Three distinct images (first, second, third) are built from one base, for the conversion
// measurement.

include { validateParameters } from 'plugin/nf-schema'

// The bash every task in `image` runs before its work (values the schema constrains). The token is
// checked for, never printed.
def task_checks(String image, String machine, boolean apptainer) {
    def checks = [
        "test \"\$(uname -m)\" = ${machine}",
        "test \"\$(cat /opt/biffix-acceptance-image)\" = ${image}",
        'test -z "${BIFFIX_INGEST_TOKEN:-}"',
    ]
    if( apptainer ) {
        // The /tmp the run's tasks share is writable: a probe file of this task's own (a fixed
        // name would let a concurrent task's `rm` take it), made and removed by separate
        // commands, each of which fails the task (in an `a && b` list, errexit ignores a failed
        // `a`).
        checks << 'test -d /.singularity.d'
        checks << 'probe=$(mktemp /tmp/.biffix-acceptance-probe.XXXXXX)'
        checks << 'rm "$probe"'
    }
    return checks.join('\n')
}

process FIRST {
    container 'first'
    cpus 1
    memory '256 MB'
    // A retry case's first attempt fails with 37 and is retried once; any other failure is final.
    maxRetries 1
    errorStrategy { task.exitStatus == 37 && params.validation_case == 'retry' && task.attempt == 1 ? 'retry' : 'terminate' }
    publishDir params.outdir, mode: 'copy'

    input:
    // Under a name of its own: an input_uri ending in first.txt would otherwise be staged as the
    // output's name.
    path data, stageAs: 'acceptance-input.txt'

    output:
    path 'first.txt', emit: first

    script:
    // A cancelled FIRST leaves a sentinel in the /tmp a single-job run's tasks share (the host's
    // scratch); the next run on that host wipes the scratch, so its FIRST must not find it.
    """
    echo "ACCEPTANCE_TASK_STARTED process=FIRST attempt=${task.attempt}" >&2
    ${task_checks('first', params.expected_machine, params.expect_apptainer)}
    test "\$(cat ${data})" = first
    case '${params.validation_case}' in
        success)
            if [ ${params.expect_apptainer} = true ]; then test ! -e /tmp/biffix-acceptance-cancel-sentinel; fi
            ;;
        retry)
            [ ${task.attempt} -gt 1 ] || exit 37
            ;;
        fail)
            exit 37
            ;;
        cancel)
            if [ ${params.expect_apptainer} = true ]; then
                printf 'cancelled\\n' > /tmp/biffix-acceptance-cancel-sentinel
            fi
            sleep 300
            ;;
    esac
    cp ${data} first.txt
    """
}

process SECOND {
    container 'second'
    cpus 1
    memory '256 MB'
    publishDir params.outdir, mode: 'copy'

    input:
    path first

    output:
    path 'second.txt', emit: second

    script:
    """
    ${task_checks('second', params.expected_machine, params.expect_apptainer)}
    test "\$(cat ${first})" = first
    printf 'second-%s\\n' ${params.expected_machine} > second.txt
    """
}

process THIRD {
    container 'third'
    cpus 1
    memory '256 MB'
    publishDir params.outdir, mode: 'copy'

    output:
    path 'third.txt', emit: third

    script:
    """
    ${task_checks('third', params.expected_machine, params.expect_apptainer)}
    printf 'third\\n' > third.txt
    """
}

workflow {
    validateParameters()
    ch_input = channel.fromPath(params.input_uri, checkIfExists: true)
    FIRST(ch_input)
    SECOND(FIRST.out.first)
    THIRD()
}
