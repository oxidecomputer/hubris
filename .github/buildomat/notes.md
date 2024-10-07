# Why are there so many jobs?

We need to build many images. Doing it in one job requires building each in
sequence. Separate buildomat jobs for each image builds in parallel and
mimimizes waiting time

# Could you add the parallelization within a buildomat job?

We could! That does not match what we do currently for our workflow. Part
of the point of CI/testing is to use the flows that already exist as
much as possible. Someone interested in this would need to profile our
buildomat jobs.
