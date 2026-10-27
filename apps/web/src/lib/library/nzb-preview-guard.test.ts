import { describe, expect, it } from 'vitest';
import { prescanNzbProlog } from './nzb-preview.js';

// Builder-owned tests for the independently specified conservative markup ceiling.
describe('bounded pre-excision literal markup scan', () => {
	it('permits exactly 150000 literal openers, rejects the 150001st', () => {
		const at = `<nzb>${'<m/>'.repeat(149998)}</nzb>`;
		const over = `<nzb>${'<m/>'.repeat(149999)}</nzb>`;
		expect(prescanNzbProlog(at)).toEqual({ ok: true, documentText: at });
		expect(prescanNzbProlog(over)).toEqual({ ok: false, errorCode: 'NZBP_TOO_MANY_NODES' });
	});
	for (const [open, close] of [['<!--', '-->'], ['<![CDATA[', ']]>'], ['<?p ', '?>']]) {
		it(`counts literal openers inside ${open} without interpreting the region`, () => {
			const at = `<nzb>${open}${'<'.repeat(149997)}${close}</nzb>`;
			const over = `<nzb>${open}${'<'.repeat(149998)}${close}</nzb>`;
			expect(prescanNzbProlog(at)).toEqual({ ok: true, documentText: at });
			expect(prescanNzbProlog(over)).toEqual({ ok: false, errorCode: 'NZBP_TOO_MANY_NODES' });
		});
	}
	it('counts the original DOCTYPE too, not just the excised parser input', () => {
		const input = `<!DOCTYPE nzb><nzb>${'<m/>'.repeat(149998)}</nzb>`;
		expect(prescanNzbProlog(input)).toEqual({ ok: false, errorCode: 'NZBP_TOO_MANY_NODES' });
	});
	it('does not treat numeric references as markup syntax', () => {
		const input = `<nzb>${'&#60;'.repeat(200000)}</nzb>`;
		expect(prescanNzbProlog(input)).toEqual({ ok: true, documentText: input });
	});
});
