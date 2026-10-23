<script lang="ts">
  // The Data page: browse the project bucket and the registered data sources,
  // one folder at a time (`DataBrowser`, in the Lab or the Expert view). The source is in the URL
  // path; the folder and the file being viewed are in its fragment (`#path=…&file=…`), so no folder
  // or file name reaches a server log. A deploy of more than one project offers a choice of project
  // (each has its own bucket and sources), kept in the query (`?project=`, absent for the default
  // one) so links stay stable. This page reads the place off the URL and loads the projects and
  // the project's sources; the browser does the rest.
  import { page } from '$app/state';
  import { listProjects, type Project } from '$lib/api';
  import DataBrowser from '$lib/data/DataBrowser.svelte';
  import { ProjectSources } from '$lib/data/projectSources.svelte';
  import { dataLocation, requestedProject } from '$lib/data/paths';
  import { presentationFor } from '$lib/data/presentation';
  import { view } from '$lib/state/userPrefs.svelte';

  // The deploy's projects, for the choice of project; `null` until (or unless) they load, when the
  // page browses the default project as a deploy of one does.
  let projects = $state<Project[] | null>(null);
  $effect(() => {
    listProjects()
      .then((all) => (projects = all))
      .catch(() => (projects = null));
  });
  // The project the URL names (`null`: the default one), which every read names too.
  const project = $derived(requestedProject(page.url));

  // The project's sources, kept with the project they are for, so a switch never shows (or lists
  // from) the previous project's; a slow answer for a project already left is dropped.
  const projectSources = new ProjectSources();
  $effect(() => {
    projectSources.load(project);
  });
  const sources = $derived(projectSources.sources(project));
  const sourcesError = $derived(projectSources.error(project));

  const where = $derived(dataLocation(page.url, page.params.rest ?? ''));
</script>

<DataBrowser
  presentation={presentationFor(view.mode)}
  {projects}
  {project}
  {sources}
  {sourcesError}
  sourceId={where.sourceId}
  path={where.path}
  file={where.file}
/>
