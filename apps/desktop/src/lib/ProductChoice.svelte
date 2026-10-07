<script lang="ts">
  import { productImageSrc, type ProductRecord } from "$lib/desktop-api";
  export let product: ProductRecord;
  export let selected: boolean;
  export let pending = false;
  export let onToggle: (enabled: boolean) => void;
  export let onOpen: ((origin: EventTarget | null) => void) | undefined = undefined;
  export let title: string | undefined = undefined;
  $: image = productImageSrc(product);
</script>

<div class="product-option" class:selected>
  <input id={`product-choice-${product.productId}`} type="checkbox" aria-label={product.name} checked={selected} disabled={pending} on:change={(event) => { const next = event.currentTarget.checked; event.currentTarget.checked = selected; onToggle(next); }} />
  <span class="product-image">{#if image}<img src={image} alt={product.name} loading="lazy" />{:else}<span class="image-placeholder" aria-hidden="true">GR</span>{/if}</span>
  <div class="product-copy">
    {#if onOpen}<button class="product-name" {title} on:click={(event) => onOpen?.(event.currentTarget)}>{product.name}</button>{:else}<label for={`product-choice-${product.productId}`}><strong>{product.name}</strong></label>{/if}
    <slot />
  </div>
  {#if $$slots.actions}<div class="product-actions"><slot name="actions" /></div>{/if}
</div>

<style>
  .product-option { box-sizing: border-box; display: flex; align-items: center; gap: 10px; min-width: 0; padding: 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--surface); transition: border-color 160ms var(--ease-ui), background 160ms var(--ease-ui); }
  .product-option:hover { background: var(--subtle); }
  .product-option.selected { border-color: var(--accent); }
  input[type="checkbox"] { flex: none; width: 16px; height: 16px; margin: 0; accent-color: var(--accent); cursor: pointer; }
  .product-image { flex: none; display: grid; place-items: center; width: 52px; height: 52px; border-radius: 4px; overflow: hidden; background: var(--photo-surface); }
  .product-image img { width: 100%; height: 100%; object-fit: contain; mix-blend-mode: var(--photo-blend); }
  .image-placeholder { color: var(--muted); font-size: 15px; font-weight: 600; letter-spacing: -1px; opacity: .5; }
  .product-copy { display: grid; flex: 1; min-width: 0; gap: 4px; }
  .product-copy strong, .product-name { color: var(--foreground); font-size: 12px; font-weight: 550; line-height: 1.5; overflow-wrap: anywhere; }
  .product-copy label { cursor: pointer; }
  .product-name { padding: 0; min-height: 0; border: 0; background: transparent; text-align: left; font-family: inherit; cursor: pointer; }
  .product-name:hover { color: var(--accent); }
  .product-actions { display: flex; align-items: center; gap: 6px; flex: none; }
  button:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  @media (prefers-reduced-motion: reduce) { .product-option { transition: none; } }
</style>
