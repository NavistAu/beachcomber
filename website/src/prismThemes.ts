import type {PrismTheme} from 'prism-react-renderer';

// Code colours for the docs, every token at least 7:1 on its block's background
// (light #ede7d8, dark #111927). Few hues on purpose: comments, strings, keywords,
// names and numbers; everything else is the plain text colour.

export const light: PrismTheme = {
  plain: {color: '#2c2416', backgroundColor: '#ede7d8'},
  styles: [
    {types: ['comment', 'prolog', 'doctype', 'cdata'], style: {color: '#4a3f2e', fontStyle: 'italic'}},
    {types: ['string', 'char', 'attr-value', 'regex', 'inserted'], style: {color: '#0a4d33'}},
    {types: ['keyword', 'atrule', 'important', 'selector', 'deleted'], style: {color: '#5e1645'}},
    {types: ['function', 'class-name', 'tag', 'builtin'], style: {color: '#16437d'}},
    {types: ['number', 'boolean', 'constant', 'symbol'], style: {color: '#6a3500'}},
    {types: ['property', 'attr-name', 'variable', 'key', 'table'], style: {color: '#7a2508'}},
    {types: ['punctuation', 'operator'], style: {color: '#2c2416'}},
  ],
};

export const dark: PrismTheme = {
  plain: {color: '#e8dfc8', backgroundColor: '#111927'},
  styles: [
    {types: ['comment', 'prolog', 'doctype', 'cdata'], style: {color: '#b5a88c', fontStyle: 'italic'}},
    {types: ['string', 'char', 'attr-value', 'regex', 'inserted'], style: {color: '#9be7b4'}},
    {types: ['keyword', 'atrule', 'important', 'selector', 'deleted'], style: {color: '#ff9ec7'}},
    {types: ['function', 'class-name', 'tag', 'builtin'], style: {color: '#8fd3ff'}},
    {types: ['number', 'boolean', 'constant', 'symbol'], style: {color: '#f5c27a'}},
    {types: ['property', 'attr-name', 'variable', 'key', 'table'], style: {color: '#f2b48a'}},
    {types: ['punctuation', 'operator'], style: {color: '#e8dfc8'}},
  ],
};
