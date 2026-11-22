// Android 10 WebView 需要这两个方法来加载终端的异步资源和渲染 Svelte 模板。
import 'core-js/es/promise/all-settled';
import 'core-js/es/string/replace-all';
import App from './App.svelte';
import './app.css';
import { mount } from 'svelte';

const app = mount(App, { target: document.getElementById('app')! });

export default app;
