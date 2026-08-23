# Problem Statement

Cooklang provides a portable, plain-text recipe format and an existing self-hosted web experience, but it does not provide the collaborative workflow needed for a group of non-technical users to collectively maintain recipes over time.

I want a self-hosted recipe platform where friends and family can:

- create and share recipes;
- collaboratively edit recipes;
- see who changed a recipe and how;
- review and accept suggested changes;
- create independent Variations while preserving their relationship to the source Recipe;
- organize Recipes into Cookbooks;
- preserve exact historical Versions;
- use the system without knowing Git, Forgejo, branches, commits, pull requests, repositories, or submodules.

I do **not** want the project to recreate functionality that already exists in Git or an open-source forge.

The platform should instead map recipe-oriented concepts onto existing Git and Forgejo primitives and provide a purpose-built cooking interface over them.

The first deployment is intended for a small circle of friends and family on a self-hosted instance. The architecture should remain compatible with broader use later without prematurely building for scale, federation, organizations, or hosted multi-tenancy.

# Solution

Build a self-hosted Rust web application that presents a non-technical Recipe and Cookbook interface while using Forgejo and Git as the authoritative backend.

The fundamental mappings are:

- Recipe → Git repository
- Version → Git commit
- History → published commits on `main`
- Variation → Forgejo fork
- Suggestion → Forgejo pull request
- Discussion → Forgejo issue
- Favorite → Forgejo star
- Notify me → Forgejo watch
- Cookbook → Git superproject
- Recipe in Cookbook → Git submodule
- Pinned Recipe → submodule fixed at its current revision
- Following Recipe → submodule configured to follow the Recipe's `main`
- Recipe ownership and permissions → Forgejo repository permissions
- User identity and authentication → Forgejo identity
- Public/Private visibility → Forgejo repository/profile visibility

Cooklang remains the canonical Recipe format. Each Recipe repository contains a `recipe.cook` file and may contain one thumbnail image in JPEG, PNG, or WebP format.

The application does not maintain a duplicate authoritative Recipe database. Durable domain state remains in Forgejo and Git. Local state is limited to operational information such as sessions, encrypted OAuth credentials, temporary Git workspaces, and rebuildable indexes.

The normal interface uses cooking terminology. Git and Forgejo remain available through an **Open in Forgejo** advanced escape hatch rather than being recreated inside the Recipe Platform.

# User Stories

1. As a user, I want to sign in with my Forgejo-backed account, so that I have one identity across Recipes, Cookbooks, Suggestions, and Git history.

2. As a first-time user, I want to land on a clear Recipes page, so that I immediately understand what I can do.

3. As a first-time user with no Recipes, I want to see actions for creating a Recipe, exploring public Recipes, and creating a Cookbook, so that I can begin without a separate onboarding wizard.

4. As a user, I want to create a Recipe by entering a title and Cooklang source, so that I can create a Recipe without interacting with Git.

5. As a user, I want to create a Recipe by uploading a `.cook` file, so that existing Cooklang Recipes can be used directly.

6. As a user, I want Cooklang text entry and `.cook` upload to be mutually exclusive creation modes, so that it is always clear which content will be used.

7. As a user, I want the Recipe title field to edit Cooklang title metadata directly, so that there is no duplicate platform-specific title.

8. As a user, I want to optionally upload a thumbnail when creating a Recipe, so that the initial Recipe is visually recognizable.

9. As a user, I want JPEG, PNG, and WebP thumbnails to be supported, so that I can use common image formats including efficient WebP images.

10. As a user, I want Recipe creation to result in one initial Version, so that the Recipe starts with clean History.

11. As a user, I want a title-only Recipe to be valid, so that I can create an unfinished Recipe and expand it later.

12. As a user, I want invalid Cooklang to prevent publishing, so that the normal interface does not create broken published Recipes.

13. As a user, I want Cooklang warnings to be visible without blocking publishing, so that I can make an informed decision about non-fatal issues.

14. As a user, I want to edit the raw Cooklang source, so that the MVP stays close to the canonical Recipe representation.

15. As a user, I want Cooklang syntax highlighting while editing, so that the raw format is easier to understand.

16. As a user, I want a rendered Recipe preview while editing, so that I can see what the Cooklang source produces.

17. As a user, I want my draft to autosave, so that I do not lose work.

18. As an Editor, I want autosaved work to remain outside published History until I explicitly publish it, so that History contains meaningful Versions rather than every keystroke.

19. As a Reader suggesting a change, I want my autosaved work to be durably stored in Forgejo, so that my work does not depend on browser-local storage.

20. As a Reader, I want an unfinished Suggestion to appear as **Editing in progress**, so that Forgejo pull-request terminology remains hidden.

21. As a Reader, I want to mark my Suggestion **Ready for review**, so that Editors know when I am finished.

22. As a user, I want at most one active draft for a given Recipe, so that I do not accidentally create several competing unfinished edits.

23. As a user, I want stale concurrent saves to be detected, so that another tab or device cannot silently overwrite newer draft work.

24. As a user, I want to discard an unfinished draft, so that abandoned work stops appearing as active.

25. As a user, I want unfinished drafts to remain available until I publish or discard them, so that the platform does not delete old work automatically.

26. As an Editor, I want to publish an edit as one clean Version, so that Recipe History remains understandable.

27. As an Editor, I want an optional description of my changes, so that important Versions can explain why they were made.

28. As an Editor who does not enter a description, I want the system to generate a sensible default message, so that publishing remains frictionless.

29. As a user, I want Recipe History to show published Versions only, so that drafts and implementation branches do not clutter the normal interface.

30. As a user, I want to open a historical Version, so that I can see what a Recipe looked like previously.

31. As a user, I want to compare Versions, so that I can understand what changed.

32. As a non-technical user, I want comparisons to be presented as **Changes**, so that I do not need to understand Git diffs.

33. As an Editor, I want to restore a historical Version by creating a new Version containing its contents, so that History is never silently rewritten.

34. As a user, I want direct Git pushes by authorized Forgejo users to appear in the Recipe Platform, so that the friendly interface is not a gatekeeper around the underlying repositories.

35. As a user, I want malformed Cooklang introduced through direct Git access to remain visible as a broken Recipe state, so that the platform does not hide authoritative Git history.

36. As an Editor, I want a malformed current Recipe to offer source viewing, repair, and previous-valid-Version options, so that I can recover without losing History.

37. As a user, I want to create a Variation from a Recipe, so that I can make my own independent version without changing the source Recipe.

38. As a user, I want **Create variation** to use a real Forgejo fork, so that shared Git history and native fork behavior are preserved.

39. As a user viewing an older Recipe Version, I want a Variation to begin from the Version I am actually viewing, so that my Variation matches the Recipe state I chose.

40. As a user, I want a new Variation to initially remain a complete copy with the same Recipe title, so that creating a Variation does not introduce arbitrary content changes.

41. As a user, I want Variation repository name collisions handled automatically, so that I never need to manage technical repository slugs.

42. As a user, I want Variations of Variations to be supported, so that Recipe lineage can grow naturally.

43. As a user, I want a Variation to show which Recipe it is based on, so that its provenance is understandable.

44. As a user, I want deleted or unavailable parent Recipes not to destroy their Variations, so that independent Recipes remain usable.

45. As a Variation owner, I want to see when the source Recipe has newer changes, so that I can decide whether those changes are relevant.

46. As a Variation owner, I want upstream changes never to apply automatically, so that my independent Recipe cannot change unexpectedly.

47. As a Variation owner, I want to review and apply upstream changes when Git can merge them safely, so that I can benefit from improvements to the source Recipe.

48. As a Variation owner, I want a conflicting upstream update to leave my Recipe unchanged, so that the platform never guesses how conflicting Recipe changes should be combined.

49. As a Reader, I want to suggest changes without directly editing a Recipe's current Version, so that I can contribute safely.

50. As a Reader, I want Suggestion submission to use Forgejo pull requests and AGit where practical, so that the platform reuses existing forge functionality.

51. As an Editor, I want to review a Suggestion as a clean Recipe change plus conversation, so that I do not need to understand pull requests, branches, or merge strategies.

52. As an Editor, I want to accept a Suggestion with one action, so that normal collaboration does not require a separate approval ceremony.

53. As an Editor, I want accepted Suggestions squash-merged into one published Version, so that Recipe History remains concise.

54. As an Editor, I want conflicting Suggestions to be clearly marked and blocked from friendly acceptance, so that incompatible changes cannot be silently merged.

55. As an Editor, I want to decline a Suggestion without deleting its conversation and provenance, so that past decisions remain inspectable.

56. As a Reader, I want to comment on a Suggestion, so that review can happen without editing the Recipe.

57. As a user, I want a Recipe Discussion area, so that questions and conversations can happen without modifying the Recipe.

58. As a user, I want Discussions to use Forgejo Issues underneath, so that the platform does not implement a separate discussion backend.

59. As a user, I want to Favorite a Recipe, so that I can easily find it again.

60. As a user, I want Favorite to map to Forgejo Star, so that favorites are not duplicated in application state.

61. As a user, I want to request notifications about a Recipe, so that Forgejo can inform me about activity.

62. As a user, I want **Notify me** to remain distinct from a Cookbook's **Follow updates** behavior, so that the two concepts are not confused.

63. As a user, I want to create a Cookbook with a title, description, and visibility, so that I can organize Recipes into a named collection.

64. As a user, I want a Cookbook to be a real Git repository, so that its composition has History.

65. As a user, I want the Cookbook title and description represented in its README, so that the Cookbook remains self-describing outside the Recipe Platform.

66. As a user, I want Cookbook description editing to use raw Markdown plus preview, so that no additional content format is invented.

67. As a user, I want to add an existing Recipe to a Cookbook without copying it, so that the same Recipe can appear in multiple Cookbooks.

68. As a user, I want a Recipe to belong to zero, one, or many Cookbooks, so that Cookbooks act as collections rather than ownership containers.

69. As a user, I want adding a Recipe to a Cookbook to use a real Git submodule, so that the Cookbook references an independent Recipe repository.

70. As a user, I want a Cookbook Recipe to be **Pinned** by default, so that adding it preserves the exact Version I selected.

71. As a user, I want the add-Recipe flow to explain **Keep this version** versus **Follow future updates**, so that the default does not hide important behavior.

72. As a user, I want switching a Pinned Recipe to Following to immediately update it to the current Recipe Version, so that Following means current plus future updates.

73. As a user, I want switching Following to Pinned to keep the current Version, so that Pin means stop moving from the current state.

74. As a user, I want Following Recipes to update automatically when the Recipe's `main` advances, so that a Following Cookbook actually follows.

75. As a user, I want each automatic Following update to create a Cookbook Version, so that the exact Cookbook contents remain historically reproducible.

76. As a user, I want automatic Cookbook Versions visible but visually de-emphasized, so that automation remains auditable without overwhelming human changes.

77. As a user, I want removing a Recipe from a Cookbook to remove only that reference, so that the Recipe and its other Cookbooks remain untouched.

78. As a user, I want the same Recipe repository to appear at most once in one Cookbook, so that duplicate references do not create ambiguous semantics.

79. As a user, I want two Recipes with identical visible titles to be distinguished by ownership or Variation context, so that the platform does not force unnecessary renames.

80. As a user, I want a broken Cookbook reference to remain visible, so that deletion or external Git changes do not silently rewrite Cookbook History.

81. As a user, I want unavailable Cookbook entries to clearly explain that their Recipe can no longer be retrieved, so that broken references are understandable.

82. As a user, I want a followed Recipe that becomes invalid Cooklang to continue advancing in the Cookbook, so that Following tracks authoritative `main` rather than parser validity.

83. As a user, I want Following to stop with a diagnostic if the expected `main` branch disappears, so that the application does not guess another branch.

84. As an Owner, I want a Public Recipe or Cookbook to be visible according to Forgejo's effective public/profile visibility rules, so that the Recipe Platform does not bypass Forgejo privacy.

85. As an anonymous visitor, I want to browse genuinely public Recipes and Cookbooks without creating an account, so that Public means public.

86. As an anonymous visitor, I want to view public Recipe History, so that previous published Versions follow the same visibility rules as the current Recipe.

87. As an anonymous visitor, I want to browse public user profiles containing public Recipes and Cookbooks, so that ownership and provenance remain visible.

88. As a signed-in user, I want Explore to separate Recipes and Cookbooks, so that different object types are easy to browse.

89. As a user, I want Explore sorting by recent, most Favorited, or alphabetical order, so that I can browse without an algorithmic recommendation feed.

90. As a user, I want MVP search to search visible Recipe/Cookbook titles, so that discovery works without building a large structured search engine.

91. As a user, I want my Recipes area to distinguish Mine, Shared with me, and Favorites, so that ownership, collaboration, and bookmarking remain clear.

92. As a user, I want my Cookbooks area to use the same Mine, Shared with me, and Favorites structure, so that navigation is consistent.

93. As a user, I want a Suggestions area with **Needs my review** and **My suggestions**, so that collaboration work has one understandable inbox.

94. As a user, I want Recipe cards to remain primarily culinary rather than Git-oriented, so that browsing does not resemble a source-code platform.

95. As a user, I want Recipe pages to show rendered Recipe content by default, so that raw Cooklang appears only when editing.

96. As a user, I want Recipe pages to provide History, Suggestions, Discussions, Variations, and Sharing without exposing Git terms, so that collaboration features remain approachable.

97. As a user, I want Cookbook pages to show description, Recipes, and Pinned/Following state without unnecessary organizational features, so that Cookbooks remain simple.

98. As an Owner, I want to make a Recipe Private or Public, so that access can change through Forgejo-native visibility.

99. As an Owner making a Private Recipe Public, I want explicit confirmation that anyone may view the Recipe and its previous Versions, so that access expansion is deliberate.

100. As an Owner making a Public Recipe Private, I want to see which public Cookbooks may become partially unavailable, so that I understand the impact before proceeding.

101. As an Owner, I want Public Recipe sharing to copy the normal Recipe URL, so that there is no separate unlisted-link mechanism.

102. As an Owner of a Private Recipe, I want Sharing to manage existing Forgejo users and their access, so that no duplicate invitation system is needed.

103. As an Owner, I want Private Recipe Readers listed because their read access is explicit, so that I know who can access the Recipe.

104. As an Owner of a Public Recipe, I want only people with additional privileges listed, so that a meaningless list of public Readers is avoided.

105. As an Owner, I want Reader and Editor to be the primary friendly sharing roles, so that common permissions remain simple.

106. As an advanced user, I want Forgejo's Administrator/Manager permissions to remain usable directly in Forgejo, so that the friendly UI does not need to expose every forge role.

107. As a Cookbook Owner sharing a private Cookbook, I want to be warned when recipients cannot view some referenced private Recipes, so that partial access is explicit.

108. As a Cookbook Owner, I want the option to grant Reader access to referenced private Recipes during sharing, so that access remains Forgejo-native but convenient.

109. As a Cookbook Editor adding a private Recipe, I want to see which Cookbook collaborators cannot view it, so that I can grant access, add anyway, or cancel.

110. As an Owner, I want Archive to be the primary reversible lifecycle action, so that normal cleanup does not destroy History.

111. As an Owner, I want permanent Recipe deletion to remain available with an impact report, so that destructive control is retained.

112. As an Owner deleting a Recipe, I want to see affected Cookbooks, Variations, and open Suggestions, so that I understand the consequences.

113. As an Owner deleting a Recipe, I want existing Cookbook references left intact rather than silently rewritten, so that historical Git state is preserved.

114. As an Owner deleting a Recipe, I want Variations to remain untouched, so that deletion never cascades through Recipe lineage.

115. As an Owner deleting a Cookbook, I want all referenced Recipes to remain untouched, so that deleting a collection cannot delete independent Recipes.

116. As an advanced user, I want **Open in Forgejo**, so that unsupported or low-level repository operations remain available without being rebuilt.

117. As an advanced user, I want ordinary Git clone/push access to the same repositories, so that the platform remains interoperable rather than becoming a closed Recipe database.

118. As an advanced user, I want repository topics to opt existing repositories into the Recipe Platform, so that compatible Forgejo repositories can be recognized without an import workflow.

119. As an advanced user, I want removing the Recipe/Cookbook topics in Forgejo to remove the repository from the Recipe UI, so that Forgejo remains authoritative.

120. As an advanced user, I want unusual extra files preserved even if the Recipe UI does not understand them, so that the friendly interface never destroys valid Git content.

121. As an advanced user, I want unsupported Forgejo state diagnosed rather than normalized, so that external Git/Forgejo changes are respected.

122. As a cook, I want temporary serving scaling without modifying the stored Recipe, so that viewing preferences do not create Versions.

123. As a cook, I want compatible unit conversions to remain temporary display transformations, so that the canonical Cooklang source is unchanged.

124. As a cook, I want interactive timers when existing Cooklang/CookCLI functionality makes them inexpensive to support, so that useful Cooklang behavior can be reused.

125. As a privacy-conscious user, I want my real email address hidden from public Recipe History by default, so that Git attribution does not unnecessarily reveal personal contact information.

126. As a self-hoster, I want no external telemetry by default, so that the installation does not phone home.

127. As a self-hoster, I want static assets served locally, so that the application works on a LAN and does not leak page views to CDNs.

128. As a self-hoster, I want a Docker Compose deployment containing the Recipe Platform and supported Forgejo LTS version, so that initial installation is straightforward.

129. As a self-hoster, I want SQLite to be sufficient for the MVP, so that I do not need PostgreSQL, Redis, or a queue system for a small deployment.

130. As a self-hoster, I want an administrator bootstrap command rather than a large graphical installer, so that integration configuration is reproducible without building another administration product.

131. As a self-hoster, I want whole-instance backups to preserve Forgejo users, permissions, Recipes, Cookbooks, forks, Suggestions, Discussions, and History, so that repository-only backups do not lose collaboration state.

132. As a self-hoster, I want deliberate LTS upgrades with backups and compatibility checks, so that Forgejo changes do not silently break the adapter.

133. As a self-hoster, I want a health endpoint and diagnostics, so that I can distinguish Recipe Platform, Forgejo, webhook, parser, automation, and reconciliation failures.

134. As a self-hoster, I want reconciliation on startup and on demand, so that missed webhooks cannot permanently desynchronize rebuildable indexes.

135. As a self-hoster, I want Forgejo unavailability surfaced clearly, so that stale cached Recipe state is not presented as authoritative.

136. As a developer, I want Forgejo accessed only through supported APIs, OAuth, webhooks, and Git protocols, so that the Recipe Platform is not coupled to Forgejo database internals.

137. As a developer, I want the Recipe Platform never to mount or manipulate Forgejo's internal Git storage, so that Forgejo remains the authoritative repository host.

138. As a developer, I want Git operations behind a replaceable adapter, so that the MVP Git CLI implementation can later be replaced without changing the Recipe model.

139. As a developer, I want the MVP Git executor to use temporary local clones/workspaces rather than authoritative local repositories, so that Forgejo remains recoverable source of truth.

140. As a developer, I want the application to use the real Git implementation for Git semantics, so that submodules, merges, refs, and AGit are not reimplemented incorrectly.

141. As a developer, I want Forgejo concepts handled through Forgejo and Git concepts handled through Git, so that the adapter respects the ownership boundary of each system.

142. As a developer, I want human actions attributed to the actual Forgejo user, so that Recipe History remains meaningful outside the friendly interface.

143. As a developer, I want genuine automated Following updates attributed to a dedicated Recipe Platform automation identity, so that automation is not falsely attributed to humans.

144. As a developer, I want a separate read-only integration identity for reconciliation where required, so that background indexing does not depend on user sessions or broad bot write privileges.

145. As a developer, I want local Recipe Platform state to be rebuildable wherever possible, so that deleting caches cannot destroy Recipe-domain state.

146. As a developer, I want partial multi-step failures represented as incomplete native Forgejo/Git state that can be retried or diagnosed, so that the app does not need a duplicate authoritative workflow database.

147. As a developer, I want structured local logs with secrets and Recipe contents redacted by default, so that operations are diagnosable without leaking credentials or user data.

148. As a developer, I want the application to reuse `cooklang-rs` directly, so that Cooklang parsing, warnings, errors, scaling, and extensions stay aligned with the canonical Rust ecosystem.

149. As a developer, I want to reuse suitable CookCLI components such as its CodeMirror Cooklang editor where practical, so that existing open-source work is not unnecessarily recreated.

150. As a developer, I want upstream contributions to CookCLI or `cooklang-rs` to remain opportunistic rather than blocking product work, so that the primary goal remains building a platform we want to use.

# Implementation Decisions

1. **Greenfield status**
   - No Recipe Platform repository, ADRs, domain glossary files, or existing codebase were available during specification.
   - The vocabulary in this specification is therefore the agreed domain vocabulary established during design: Recipe, Cookbook, Version, History, Variation, Suggestion, Discussion, Reader, Editor, Owner, Pinned, Following, Favorite, and Notify me.

2. **Core architectural rule**
   - If Git or Forgejo already provides a suitable primitive, the Recipe Platform maps that primitive into cooking terminology instead of creating another authoritative implementation.

3. **Source of truth**
   - Forgejo is authoritative for identity, authentication, repository ownership, repository permissions, visibility, forks, pull requests, issues, stars, watches, repository lifecycle, and other forge-level state.
   - Git repositories are authoritative for Recipe/Cookbook content and History.
   - The Recipe Platform must not maintain duplicate authoritative Recipe-domain records.

4. **Recipe mapping**
   - A Recipe is one Forgejo-hosted Git repository.
   - `main` is the current published Recipe.
   - A Version is a published Git commit reachable from `main`.
   - History is the collection of published Versions.
   - Friendly restore creates a new Version with historical contents; it never rewrites `main` History.

5. **Recipe repository convention**
   - A supported Recipe contains exactly one canonical `recipe.cook`.
   - It may contain zero or one supported thumbnail: `recipe.jpg`, `recipe.png`, or `recipe.webp`.
   - Additional arbitrary files may exist and must be preserved.
   - Multiple supported thumbnails are treated as an ambiguous state rather than silently prioritized.
   - Normal UI operations maintain the zero-or-one-thumbnail invariant.

6. **Cooklang**
   - Cooklang is the canonical Recipe representation.
   - `cooklang-rs` is used directly.
   - Canonical parser extensions are enabled.
   - Parser dependency versions are pinned to Recipe Platform releases.
   - Parser upgrades never automatically rewrite stored Recipes.
   - Errors block friendly publishing/creation.
   - Warnings are displayed but do not block publishing.
   - External invalid Cooklang remains legitimate Git state and is represented as an invalid Recipe state.

7. **Recipe title**
   - User-facing Recipe title comes from Cooklang title metadata.
   - Forgejo repository name is a stable technical slug generated from the initial title.
   - Normal Recipe renaming modifies Cooklang metadata but does not automatically rename the repository.

8. **Recipe creation**
   - Friendly creation requires title, visibility, and either raw Cooklang input or `.cook` upload.
   - Raw input and file upload are mutually exclusive modes.
   - Thumbnail upload is optional.
   - Public is selected by default.
   - Creation produces one initial commit.
   - Title-only Cooklang is allowed.
   - Repository slug collisions are resolved automatically.

9. **Thumbnail handling**
   - JPEG, PNG, and WebP are first-class formats.
   - Friendly uploads are limited to 5 MB.
   - Replacing an image with a different format removes the previous supported thumbnail in the same Version.
   - No automatic WebP conversion or image optimization is included in MVP.

10. **Editor**
    - MVP editor is raw Cooklang rather than structured/WYSIWYG.
    - CodeMirror 6 Cooklang syntax highlighting is reused/adapted from CookCLI where practical.
    - Rendered preview is available while editing.
    - The editor does not automatically reformat Cooklang.

11. **Drafts**
    - Each user has at most one active draft per Recipe.
    - Editor drafts use temporary Git branches and mutable/amended draft commits.
    - Reader drafts use a Forgejo pull request created through AGit where possible.
    - Reader WIP state maps to Forgejo-native WIP pull-request naming/state conventions.
    - Reader autosaves update the same Suggestion rather than creating many independent Suggestions.
    - Drafts do not expire automatically.
    - Concurrent stale writes are rejected using the expected Git ref/head state.

12. **Publishing**
    - Editor publishing turns the latest draft into one clean Version on `main`.
    - Optional human change notes become the Version description/commit message.
    - Completed temporary Editor branches are removed after successful publishing.
    - If `main` changed while a user was editing, native Git integration is attempted; conflicts leave published state unchanged.

13. **Suggestions**
    - Suggestion maps to a Forgejo pull request.
    - AGit is preferred for Reader submission if possible.
    - Pull request remains the durable review object.
    - Friendly Suggestion review exposes change diff, conversation, Accept, Decline, and Comment.
    - Accept uses squash merge for MVP.
    - Decline closes rather than deletes the Forgejo pull request.
    - Conflicting Suggestions cannot be accepted through the friendly UI.

14. **Variations**
    - Variation maps to a Forgejo fork.
    - Forks of forks are supported.
    - Forgejo's direct fork relationship is authoritative for lineage.
    - No custom fork-point tag or metadata is introduced for MVP.
    - When creating a Variation from a historical Recipe Version, the new fork's `main` is moved to the Version being viewed before it is exposed.
    - The Variation initially retains the exact Recipe content/title.
    - New Variations are user-owned in MVP.
    - Variations inherit source visibility initially.
    - Later visibility changes remain independent Forgejo repository decisions.

15. **Variation synchronization**
    - Variation divergence/status uses Forgejo/Git fork state rather than an application-specific lineage engine.
    - Upstream changes are never automatically applied to Variations.
    - Clean upstream merges may be applied through a friendly **Update from original** operation.
    - Merge conflicts leave both Recipes unchanged and direct advanced users to Forgejo.

16. **Cookbook mapping**
    - A Cookbook is a Git repository acting as a superproject.
    - Its Recipes are independent Git repositories represented as submodules.
    - `main` is the current published Cookbook composition.
    - Cookbook title is the first H1 in `README.md`.
    - Remaining README content is the Cookbook description.
    - No `cookbook.yaml` exists in MVP.
    - `.gitmodules` and gitlinks represent Recipe references.
    - No section/group/order metadata is stored in MVP.

17. **Cookbook creation**
    - Friendly creation asks for title, description, and Public/Private visibility.
    - Initial repository contains `README.md`.
    - `.gitmodules` appears only once at least one Recipe is added.
    - Cookbook README is edited as raw Markdown with preview.

18. **Cookbook Recipe ordering**
    - No persistent manual ordering is supported.
    - Default presentation is alphabetical by Recipe title.
    - UI sort options may exist without changing repository state.

19. **Pinned and Following**
    - Pinned is the default when adding a Recipe.
    - A Pinned submodule has no tracked branch in platform convention.
    - A Following submodule declares `branch = main`.
    - Gitlink always records the exact currently selected commit in either mode.
    - Switching Pinned → Following immediately moves to current `main`.
    - Switching Following → Pinned keeps the current Version.
    - Following automatically advances when Recipe `main` changes.
    - Each upstream Recipe update creates one Cookbook Version.
    - Automatic Versions remain visible but may be grouped/de-emphasized in UI.

20. **Cookbook reference behavior**
    - Adding a Recipe does not modify that Recipe repository except explicit permission changes approved by the user.
    - Removing a Recipe removes only the submodule reference.
    - One Recipe repository may appear at most once in a given Cookbook.
    - Submodule path is generated from the Recipe repository slug at first insertion and remains stable.
    - Broken references remain visible rather than being deleted automatically.
    - Deleting a Recipe does not automatically repair or remove Cookbook references.
    - Advanced repository rename/transfer does not automatically repair Cookbook submodule URLs in MVP.

21. **Visibility**
    - Only Forgejo-native Public and Private visibility are supported.
    - Unlisted sharing is out of scope.
    - Public is default for new Recipes/Cookbooks.
    - Forgejo user-profile visibility is also respected; the Recipe Platform must not bypass it.
    - Public History is publicly accessible when Forgejo considers the repository anonymously visible.
    - Public → Private warns about affected public Cookbooks but remains allowed.
    - Private → Public requires confirmation that Recipe and previous Versions become publicly readable.

22. **Permissions**
    - Forgejo repository permissions remain authoritative.
    - Friendly UI primarily exposes Reader and Editor plus implicit Owner.
    - Reader maps to Forgejo Read.
    - Editor maps to Forgejo Write.
    - Forgejo Administrator/Manager remains an advanced permission available through Forgejo rather than the normal MVP UI.
    - Cookbook permissions use the same model.
    - Cookbook membership never grants Recipe permissions automatically.

23. **Private Cookbook access mismatches**
    - When sharing a private Cookbook, the UI reports referenced Recipes that the recipient cannot access.
    - The owner may explicitly grant Reader access to those Recipes, share anyway, or cancel.
    - When adding a private Recipe to a shared Cookbook, the UI reports collaborators who cannot access it and offers explicit grant/add-anyway/cancel choices.
    - Grants are normal Forgejo collaborator permissions.

24. **Ownership**
    - Recipes and Cookbooks are personally owned in MVP.
    - Forgejo Organizations/Teams are reserved for a later Group/family ownership feature.
    - Friendly ownership transfer is out of scope; use Forgejo directly.

25. **Repository classification**
    - Forgejo topics are the opt-in adapter markers.
    - Recipe repositories use `cooklang` + `recipe`.
    - Cookbook repositories use `cooklang` + `cookbook`.
    - The Recipe Platform does not scan arbitrary Forgejo repositories and infer intent solely from files.
    - Removing required topics externally removes the repository from the Recipe UI.

26. **Discovery**
    - Public Forgejo repository state is the public catalog.
    - Explore separates Recipes and Cookbooks.
    - Public browsing does not require login.
    - Search is title-only for MVP and uses the user-visible Recipe/Cookbook title.
    - Sorting supports recent, most Favorited/starred, and alphabetical.
    - No recommendation feed exists.

27. **User areas**
    - Primary navigation: Recipes, Cookbooks, Explore, Suggestions, Profile.
    - Default authenticated landing page: Recipes.
    - Recipes and Cookbooks each have Mine, Shared with me, Favorites.
    - Suggestions has Needs my review and My suggestions.

28. **Profiles**
    - Forgejo user is the Recipe Platform user.
    - No duplicate profile record is maintained.
    - Public profiles display public Recipe/Cookbook repositories according to Forgejo visibility.
    - Owner attribution should use wording such as **Owned by** where authorship could otherwise be misleading.

29. **Discussions**
    - Discussion maps to Forgejo Issues.
    - Suggestion conversation maps to pull-request comments.
    - No inline Recipe-step/ingredient annotation system is included.
    - If Forgejo Issues are disabled for a repository, Discussion is absent rather than re-enabled automatically.

30. **Favorite and notifications**
    - Favorite maps to Forgejo Star.
    - Notify me maps to Forgejo Watch.
    - Cookbook **Follow updates** remains a separate concept.
    - The Recipe Platform does not implement an independent notification inbox or SMTP subsystem in MVP.

31. **Sharing**
    - Public Recipe Share copies the normal Recipe URL.
    - Private sharing manages existing Forgejo users.
    - No Recipe Platform email invitation system exists.
    - User registration/account invitation remains Forgejo administrator responsibility.

32. **Deletion and archive**
    - Archive maps to Forgejo repository archive and is the ordinary lifecycle operation.
    - Permanent Delete remains available through the Recipe UI with strong impact confirmation.
    - Recipe Delete never cascades into Variations.
    - Cookbook Delete never deletes referenced Recipes.
    - Deleted parent Recipes leave Variations usable with unavailable lineage.
    - Active Cookbook references may become broken rather than automatically rewritten.

33. **External Git/Forgejo changes**
    - Direct Git and Forgejo operations are legitimate.
    - The Recipe Platform diagnoses unsupported/malformed state but does not silently normalize it.
    - Missing `main`, missing `recipe.cook`, malformed Cooklang, stale submodule URLs, extra files, custom branches, manual pull requests, and other advanced state may make friendly features partially unavailable.
    - **Open in Forgejo** is the escape hatch.

34. **No direct Forgejo internals**
    - The Recipe Platform never queries Forgejo's database directly.
    - The Recipe Platform never mounts or modifies Forgejo's repository storage.
    - Supported Forgejo APIs, webhooks, OAuth, and Git endpoints are the only integration interfaces.

35. **Authentication**
    - Forgejo is OAuth2 identity provider.
    - Recipe Platform browser receives only a secure Recipe Platform session.
    - Forgejo OAuth credentials remain server-side.
    - Internal human Git operations use HTTPS with that user's Forgejo OAuth credential.
    - Recipe Platform does not manage user SSH keys.

36. **Commit attribution**
    - Human commits use the authenticated human's effective Forgejo Git identity for Author and Committer.
    - Forgejo's private/no-reply email behavior must be respected.
    - Bundled deployment defaults to private commit emails.
    - Automatic Following commits use a clearly identified Recipe Platform automation user.
    - Commit signing is out of scope for MVP.

37. **Service identities**
    - One read-only privileged integration identity may be used for instance-wide reconciliation/indexing.
    - One ordinary automation bot gains Write access only to Cookbooks requiring Following automation.
    - Platform service identities are hidden from normal friendly sharing screens but remain visible in Forgejo.
    - Removing automation permission externally stops automation and produces a diagnostic; the app does not silently re-grant it.

38. **Local application state**
    - No authoritative domain database.
    - Persistent operational state may include sessions, encrypted Forgejo access/refresh credentials, and installation secrets.
    - Search index and Cookbook reverse-reference index are disposable and rebuildable.
    - Git workspaces/clones are disposable.

39. **Git adapter**
    - Forgejo API is used for forge-level concepts.
    - Standard Git is used for Git-level concepts.
    - MVP Git implementation executes the system Git executable from the Recipe Platform backend container.
    - Operations use short-lived local clones/workspaces.
    - Persistent mirrors may be added later only if measurements justify them.
    - Git execution sits behind a replaceable internal adapter.
    - The adapter must be replaceable later by a library, worker/service, or deeper Forgejo integration without changing domain behavior.
    - This CLI/local-clone design is explicitly accepted MVP technical debt rather than permanent architecture.

40. **Partial failure**
    - No cross-Forgejo/Git ACID transaction layer is introduced.
    - Multi-step operations are designed to be idempotent/retryable.
    - Reconciliation detects incomplete native state.
    - Incomplete setup is surfaced to the owner with retry/Open in Forgejo/delete options rather than hidden in a duplicate workflow database.

41. **Webhooks and reconciliation**
    - Bundled Forgejo configures one authenticated system webhook to the Recipe Platform.
    - Webhooks provide fast updates but are not the only synchronization mechanism.
    - Startup reconciliation and an admin Reconcile/Rebuild operation scan authoritative Forgejo/Git state and rebuild caches/indexes.

42. **Application architecture**
    - Implementation language: Rust.
    - Single server-rendered application rather than separate SPA/API.
    - Axum + Tower for HTTP/server infrastructure.
    - Askama for server-rendered templates.
    - Lightweight HTML-over-the-wire interactions are preferred for ordinary dynamic UI.
    - CodeMirror is the major client-side component.
    - Node/npm are acceptable build-time dependencies only.
    - Runtime remains a Rust application plus required static assets and Git executable.

43. **CookCLI reuse**
    - Do not fork CookCLI as the Recipe Platform product.
    - Reuse/adapt useful MIT-licensed CookCLI components where appropriate.
    - Highest-value reuse includes CodeMirror Cooklang syntax/editor behavior and cooking-oriented rendering functionality.
    - Generic upstream contributions are welcome but opportunistic; they must not block Recipe Platform development.

44. **Responsive UI**
    - Desktop and mobile browser use are equally important.
    - No native mobile app is part of MVP.
    - Current evergreen Firefox, Chromium-family browsers, and Safari are supported.

45. **Accessibility**
    - Semantic HTML, keyboard accessibility, form labeling, focus behavior, and suitable contrast are MVP quality requirements.

46. **Rendering security**
    - User-controlled Cooklang and Markdown output is sanitized before HTML rendering.
    - Dangerous URL schemes are removed.
    - Arbitrary executable HTML/scripts/iframes/forms are not trusted.
    - External links may be displayed.
    - Remote embedded images are not automatically loaded in MVP to avoid tracking/privacy issues.
    - UI assets are served locally without required external CDN resources.

47. **Size limits**
    - Friendly `.cook` creation/editing limit: 1 MB.
    - Friendly `README.md` editing limit: 1 MB.
    - Friendly thumbnail upload limit: 5 MB.
    - External Git state above these limits is preserved; friendly rendering/editing may display a safe unsupported/too-large state.

48. **Deployment**
    - Supported MVP deployment is Docker Compose.
    - Bundled backend is Forgejo 15.x LTS.
    - SQLite is the default database choice for the expected small deployment.
    - No Redis, queue infrastructure, PostgreSQL requirement, object storage, Kubernetes requirement, or Forgejo Actions runner.
    - Forgejo remains reachable as the advanced interface.
    - The administrator is expected to be technical; the project does not build a replacement Forgejo administration UI.

49. **Routing/deployment boundary**
    - The bundled stack is the supported MVP configuration.
    - External existing Forgejo installations are future-compatible but unsupported in MVP.
    - Reverse proxy/TLS is not a mandatory bundled service.
    - Deployment documentation may provide common proxy examples.

50. **Backup and upgrade**
    - Supported backup is whole-instance backup, not Recipe-repository-only backup.
    - Forgejo users, ACLs, fork relationships, PRs, Issues, stars, and other forge state are part of recoverable product state.
    - Upgrades are deliberate and tied to tested Forgejo LTS compatibility.
    - Automatic major-version Forgejo upgrades are not performed.

51. **Health and failure behavior**
    - `/health`-style status is provided for orchestration.
    - Admin diagnostics report Recipe Platform, Forgejo, integration, webhook, reconciliation, automation, and parser health.
    - Forgejo unavailability causes visible temporary-unavailable state.
    - The Recipe Platform does not accept edits while authoritative backend is unavailable.
    - Stale cached domain state is not presented as current truth.

52. **Logging and telemetry**
    - No external analytics/telemetry by default.
    - No tracking pixels or automatic crash-reporting service.
    - Structured local logs are used.
    - OAuth tokens, session secrets, Git credentials, and Recipe contents are redacted/not logged by default.

53. **License**
    - Recipe Platform is AGPLv3.
    - This permits reuse of MIT-licensed CookCLI components while keeping modified network-served Recipe Platform derivatives subject to AGPL obligations.

54. **Repository organization**
    - Recipe Platform implementation itself is one repository and one deployable application for MVP.
    - No separate Git worker service is built initially.
    - Internal boundaries must permit later extraction if needed.

# Testing Decisions

1. **Primary seam**
   - The principal acceptance seam is **Recipe Platform ↔ real Forgejo/Git**.
   - Tests exercise the highest available application/user-facing interface against a disposable supported Forgejo 15 LTS instance and real Git repositories.
   - This is intentionally preferred over mocking Forgejo because the most important risks are whether the proposed mappings actually behave like Forgejo and Git.

2. **What makes a good test**
   - Tests assert externally observable product behavior rather than internal implementation details.
   - A test should care that creating a Variation produces the correct independent Recipe and lineage behavior, not which Rust helper executed `git`.
   - A test should care that accepting a Suggestion creates one published Version and closes/merges the underlying review correctly, not how HTTP calls are arranged internally.
   - A test should care that Following moves the Cookbook reference and creates History, not the local clone directory or Git CLI invocation sequence.
   - Tests should continue to pass if the Git CLI adapter is later replaced by libgit2, a separate worker, or another compliant implementation.

3. **Forgejo integration coverage**
   - OAuth identity/authentication.
   - Repository creation.
   - Public/private visibility.
   - Repository topics.
   - Collaborator permissions.
   - Stars and watches.
   - Fork creation and forks of forks.
   - Pull-request creation through AGit where supported.
   - WIP/Ready Suggestion lifecycle.
   - Squash merge.
   - Closing/declining Suggestions.
   - Issues/Discussions.
   - Repository archive and deletion.
   - System webhook delivery and reconciliation after missed webhook events.
   - Profile visibility effects.
   - Service-account permissions.
   - Out-of-band Forgejo changes.

4. **Real Git integration coverage**
   - Recipe initial commit.
   - Draft branch creation/update.
   - Clean publication to `main`.
   - Concurrent/stale ref detection.
   - Restore-as-new-Version.
   - Version comparison.
   - Historical Variation creation.
   - Clean upstream Variation merge.
   - Variation merge conflict behavior.
   - Submodule creation/removal.
   - Pinned submodule behavior.
   - Following submodule configuration.
   - Automatic submodule advancement.
   - Broken submodule state.
   - Missing/renamed `main`.
   - Force-pushed/rewritten upstream History.
   - External Git pushes.

5. **Cooklang unit/integration coverage**
   - Valid parsing.
   - Errors versus warnings.
   - Extensions.
   - Title extraction/editing.
   - Rendered output.
   - Scaling.
   - Unit conversion behavior used by MVP.
   - Invalid current Recipe presentation.
   - Parser-version upgrade regression tests.
   - No unintended source reformatting.

6. **Recipe repository convention tests**
   - `recipe.cook` required.
   - JPEG thumbnail.
   - PNG thumbnail.
   - WebP thumbnail.
   - No thumbnail.
   - Multiple thumbnails reported as ambiguous.
   - Unsupported extra files preserved.
   - Oversized friendly uploads rejected.
   - Oversized externally pushed state handled safely.

7. **Cookbook tests**
   - README title extraction.
   - README description rendering/sanitization.
   - Empty Cookbook.
   - Recipe addition.
   - Duplicate Recipe rejection.
   - Stable generated submodule path.
   - Path collision handling.
   - Recipe removal.
   - Delete Cookbook without deleting Recipes.
   - Alphabetical rendering.
   - Pinned/Following transition behavior.

8. **Permission tests**
   - Public anonymous view.
   - Limited Forgejo profile behavior.
   - Private Reader access.
   - Editor direct publishing.
   - Reader Suggestion without direct publish.
   - Cookbook access not granting Recipe access.
   - Explicit private Recipe access grants during sharing.
   - Adding inaccessible private Recipe to Cookbook.
   - Removal of automation bot permission.

9. **Deletion tests**
   - Archive Recipe.
   - Unarchive if supported by exposed workflow.
   - Permanent Recipe delete impact confirmation.
   - Existing Variation remains usable.
   - Cookbook reference remains broken.
   - Open Suggestions disappear/close according to Forgejo deletion semantics.
   - Cookbook delete leaves Recipes intact.

10. **Security tests**
    - Markdown/Cooklang rendered HTML sanitization.
    - Dangerous URL schemes.
    - Raw HTML handling.
    - Remote images not automatically loaded.
    - OAuth tokens absent from HTML/client responses.
    - OAuth tokens absent from Git URLs and logs.
    - No credentials persisted in temporary Git configs.
    - Commit email privacy respected.
    - Session persistence and invalidation.
    - Size-limit enforcement.

11. **Reconciliation tests**
    - Destroy rebuildable indexes and recover them.
    - Miss webhook events, then reconcile successfully.
    - Externally remove repository topics.
    - Externally change visibility.
    - Externally disable Issues.
    - Externally modify submodules.
    - Externally delete a referenced Recipe.
    - Externally alter branch conventions.
    - No automatic mutation during reconciliation unless the behavior is an explicitly defined automation such as Following.

12. **Deployment tests**
    - Supported Docker Compose stack starts from empty state.
    - Bootstrap configures required Forgejo integration.
    - Health checks report correct status.
    - Recipe Platform survives restart with sessions/credentials.
    - Forgejo outage is surfaced visibly.
    - Reconciliation succeeds after outage.
    - Backup/restore smoke test preserves forge/domain behavior.

13. **Browser/accessibility tests**
    - Critical flows work on current Chromium, Firefox, and Safari/WebKit equivalents where practical.
    - Mobile viewport Recipe reading/editing.
    - Keyboard navigation for principal controls.
    - Accessible labels and focus behavior for dialogs/forms.
    - No critical action requires pointer-only interaction.

14. **Test data and fixtures**
    - Favor small real Cooklang fixtures and real Git repositories over large mocked object graphs.
    - Use fixtures for valid, warning-producing, invalid, extended, and externally modified Cooklang.
    - Use multiple Forgejo users to test Reader/Editor/Owner and private/public behavior.

15. **Prior art**
    - There is no available Recipe Platform codebase or existing test suite from which to reuse test patterns.
    - Therefore the first implementation should establish the real Forgejo-container integration harness as the primary testing convention.
    - CookCLI and `cooklang-rs` upstream tests may be consulted for Cooklang/parser/editor behavior, but Recipe Platform collaboration tests are new.

# Out of Scope

The following are explicitly outside the MVP:

- Structured/WYSIWYG Recipe editor.
- Semantic Recipe-aware diff.
- Meal planning.
- Shopping lists.
- Pantry/inventory management.
- Nutrition analysis.
- Recipe website scraping.
- PDF/text-to-Recipe extraction.
- Algorithmic recommendation feed.
- Public-growth or social-network mechanics beyond public discovery, Favorites, Variations, Discussions, and profiles.
- Moderation platform for large public communities.
- Hosted SaaS/multi-tenant deployment.
- Organizations/Families/Groups in the friendly UI.
- Forgejo Organizations/Teams mapping in MVP.
- Recipe federation.
- Cross-instance stable Recipe identity.
- Recipe-platform-specific federation protocol.
- Supported attachment to arbitrary existing Forgejo installations.
- Unlisted share links.
- Email invitation system.
- Recipe Platform SMTP subsystem.
- Recipe Platform notification inbox.
- Generic repository file browser.
- Generic Forgejo administration UI.
- Friendly ownership transfer.
- Friendly account deletion/administration.
- Branch protection configuration.
- Force-push/history rewriting controls.
- Advanced branch management.
- Detailed Forgejo review workflows.
- Custom merge/conflict resolver.
- Inline ingredient/step comments.
- Cookbook sections.
- Manual persistent Cookbook ordering.
- Cookbook forking in the friendly UI.
- Arbitrary Forgejo package/project/wiki/Actions UI.
- Forgejo Actions runner.
- Separate Git worker service.
- Persistent Git mirror optimization unless measured performance requires it.
- Redis.
- Required PostgreSQL.
- Object storage.
- Kubernetes assumptions.
- External CDN requirements.
- External telemetry.
- Commit signing.
- Automatic image conversion/optimization.
- Automatic repository rename/submodule repair.
- Automatic deletion cleanup across Cookbooks.
- Custom Recipe UUID or original fork-point metadata.
- Public Recipe Platform API for third-party integrations.
- Upstreaming CookCLI/cooklang-rs improvements as a prerequisite or primary project objective.

# Further Notes

- The design deliberately favors **native Git/Forgejo state over perfectly controlled Recipe Platform state**. Advanced users can put repositories into states the friendly UI cannot fully handle. The correct response is diagnosis and **Open in Forgejo**, not silent normalization.

- The Recipe Platform should remain a **domain-specific client of Forgejo**, not gradually evolve into its own forge.

- Forgejo's public interfaces form an architectural boundary. The platform must remain capable, in principle, of moving from bundled Forgejo to an external instance without replacing the Recipe/Cookbook domain model.

- The MVP Git implementation using the system Git executable and temporary clones is accepted as a pragmatic compromise. It must remain isolated behind a replaceable adapter and documented as technical debt.

- Public Recipe licensing is not enforced in the MVP. Public does not imply a specific content license.

- Public user-profile behavior follows Forgejo rather than a separate Recipe Platform privacy model.

- Federation is a desired future direction because Forgejo supports federation work, but the MVP must not introduce custom federation state in anticipation of it.

- Generic improvements to CookCLI or `cooklang-rs` may be contributed upstream when convenient, but shipping and using this Recipe Platform is the priority.

- The first validation environment is a small real group of friends/family. Complexity should be earned from actual use rather than predicted in advance.

- Suggested issue title: **Forgejo-backed collaborative Cooklang Recipe Platform MVP**

- Required issue label: **`ready-for-agent`**

- Publication status: **blocked — no Recipe Platform issue tracker is currently available.**