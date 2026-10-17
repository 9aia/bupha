import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'bupha',
  description: 'headless CI — the oxpecker on your repo',
  base: '/bupha/',
  ignoreDeadLinks: [/\/api\//],
  themeConfig: {
    nav: [
      { text: 'Tutorial', link: '/tutorials/first-run' },
      { text: 'Reference', link: '/reference/cli' },
      { text: 'API', link: '/api/index.html', target: '_blank' },
      { text: 'GitHub', link: 'https://github.com/9aia/bupha' },
    ],
    sidebar: [
      {
        text: 'Tutorials',
        items: [{ text: 'Your first run', link: '/tutorials/first-run' }],
      },
      {
        text: 'How-to',
        items: [
          { text: 'Multi-repo', link: '/how-to/multi-repo' },
          { text: 'Script runner', link: '/how-to/script-runner' },
          { text: 'sccache backend', link: '/how-to/sccache-backend' },
        ],
      },
      {
        text: 'Reference',
        items: [
          { text: 'CLI', link: '/reference/cli' },
          { text: 'Config', link: '/reference/config' },
          { text: 'Releasing', link: '/reference/releasing' },
          { text: 'Crate APIs (rustdoc)', link: '/api/index.html', target: '_blank' },
        ],
      },
      {
        text: 'Explanation',
        items: [
          { text: 'Architecture', link: '/explanation/architecture' },
          { text: 'vs hosted CI', link: '/comparison' },
        ],
      },
    ],
    socialLinks: [
      { icon: 'github', link: 'https://github.com/9aia/bupha' },
    ],
  },
})
