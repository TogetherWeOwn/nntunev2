<script lang="ts">
	let { prefix, fieldId, label, value, instructions, instructionsId = `${prefix}-instructions`, rows = 4 }: {
		prefix: string;
		fieldId: string;
		label: string;
		value: string;
		instructions: string;
		instructionsId?: string;
		rows?: number;
	} = $props();
</script>

<label for={fieldId}>{label}</label>
<textarea
	id={fieldId}
	data-testid={`${prefix}-fallback`}
	readonly
	{rows}
	{value}
	aria-describedby={instructionsId}
></textarea>
<p id={instructionsId} data-testid={`${prefix}-instructions`}>{instructions}</p>
