/*
 * Taken from CookCLI.
 *
 * Copyright (c) 2021-2023 Alexey Dubovskoy
 * Licensed under the MIT License. See LICENSE-MIT-cookcli.
 *
 * Used here so that CookLangHub and CookCLI read as one family. See NOTICE.
 */
/** @type {import('tailwindcss').Config} */
module.exports = {
  darkMode: 'class',
  content: [
    "./templates/**/*.{html,js}",
    "./static/**/*.{html,js}",
    "./src/**/*.rs",
  ],
  theme: {
    extend: {
      colors: {
        'primary-orange': '#ff6b35',
        'primary-green': '#4ade80',
        'light-orange': '#fed7aa',
        'light-blue': '#dbeafe',
        'light-green': '#dcfce7',
        'light-yellow': '#fef3c7',
      },
      animation: {
        'gradient': 'gradient 3s ease infinite',
      },
      keyframes: {
        gradient: {
          '0%, 100%': {
            'background-size': '200% 200%',
            'background-position': 'left center'
          },
          '50%': {
            'background-size': '200% 200%',
            'background-position': 'right center'
          }
        }
      }
    },
  },
  plugins: [],
}