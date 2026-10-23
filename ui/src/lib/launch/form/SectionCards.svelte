<script lang="ts">
  // The form's cards in the author's section order, each with its fields. The reference dropdown
  // sits in the section the author names for it, else in a card of its own ahead of the rest.
  import type { Card } from '../form';
  import ReferenceField from '../ReferenceField.svelte';
  import { REFERENCE_FIELD } from '../referenceChoice';
  import FieldControl from './FieldControl.svelte';
  import FormCard from './FormCard.svelte';
  import { launchForm } from './launchForm.svelte';

  interface Props {
    cards: Card[];
    /** The cards' heading level: 2 in the Expert form, 3 under a wizard step's heading. */
    level?: 2 | 3;
  }
  let { cards, level = 2 }: Props = $props();
  const form = launchForm();
</script>

{#snippet referenceControl()}
  {#if form.refs}
    <ReferenceField
      refs={form.refs}
      offered={form.references.offered}
      loadError={form.references.error}
      anyRegistered={form.references.anyRegistered}
      choice={form.refChoice}
      onchoice={(c) => form.setRefChoice(c)}
      error={form.errorFor(REFERENCE_FIELD)}
    />
  {/if}
{/snippet}

{#if form.refs && !cards.some((c) => c.section.id === form.refs?.section)}
  <FormCard id="sec-reference" title={form.refs.label} {level}>
    {@render referenceControl()}
  </FormCard>
{/if}
{#each cards as card (card.section.id)}
  <FormCard
    id={`sec-${card.section.id}`}
    title={card.section.title}
    {level}
    description={card.section.description}
  >
    {#if form.refs && card.section.id === form.refs.section}{@render referenceControl()}{/if}
    {#each card.fields as f (f.name)}
      <FieldControl field={f} />
    {/each}
  </FormCard>
{/each}
