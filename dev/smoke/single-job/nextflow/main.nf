include { validateParameters } from 'plugin/nf-schema'

process UBUNTU_HELLO {
    container 'public.ecr.aws/docker/library/ubuntu:24.04'
    cpus 1
    memory '256 MB'
    publishDir params.outdir, mode: 'copy'

    output:
    path 'hello.txt', emit: greeting

    script:
    '''
    . /etc/os-release
    test "$ID" = ubuntu
    test "$VERSION_ID" = 24.04
    test -d /.singularity.d
    printf 'hello ubuntu 24.04\n' > hello.txt
    '''
}

process UBUNTU_WORLD {
    container 'public.ecr.aws/docker/library/ubuntu:24.04'
    cpus 1
    memory '256 MB'
    publishDir params.outdir, mode: 'copy'

    input:
    path greeting

    output:
    path 'world.txt', emit: result

    script:
    """
    . /etc/os-release
    test "\$ID" = ubuntu
    test "\$VERSION_ID" = 24.04
    test -d /.singularity.d
    cat ${greeting} > world.txt
    printf 'world ubuntu 24.04\n' >> world.txt
    """
}

workflow {
    validateParameters()
    UBUNTU_HELLO()
    UBUNTU_WORLD(UBUNTU_HELLO.out.greeting)
}
