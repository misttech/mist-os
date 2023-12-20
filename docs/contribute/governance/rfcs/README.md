{% include "docs/contribute/governance/rfcs/_common/_rfc_header.md" %}

# Fuchsia RFCs

The [Fuchsia RFC process](rfc_process.md)
is intended to provide a consistent and transparent path
for making project-wide, technical decisions. For example, the RFC process can
be used to evolve the project roadmap and the system architecture.

The RFC process evolves over time, and can be read here in its [detailed current
form](rfc_process.md). It is also summarized below.

## Summary of the process

- Review [when to use the process](rfc_process.md#when-to-use-the-process).
- Socialize your proposal.
- [Draft](rfc_process.md#draft) your RFC using this [template](TEMPLATE.md)
  and share with stakeholders. See [creating an RFC](create_rfc.md) and
  [RFC best practices](best_practices.md).
- As conversations on your proposal converge, and stakeholders indicate their
  support, email <eng-council@fuchsia.dev> to ask the Eng Council
  to move your proposal to [Last Call](rfc_process.md#last-call).
- After a waiting period of at least 7 days, the Eng Council will accept or
  reject your proposal, or ask that you iterate with stakeholders further.

For detailed information, follow the [RFC process](rfc_process.md).

## Summary of the process (deck)

<!-- Wrap the iframe in a div to get fixed-aspect-ratio responsive behavior -->
<!-- mdlint off(WHITESPACE_LINE_LENGTH) -->
<div style="padding-top: 62%; position: relative; width: 100%">
  <iframe
    src="https://docs.google.com/presentation/d/e/2PACX-1vT8Sofn5v3d-PP7fcBw9YTH4vukwlvscjjqKsC4eItDVp79qYbENpAKer6ZoE_bQ3vD23dwHYrBn_aP/embed?start=false&loop=false&delayms=3000"
    frameborder="0" width="480" height="299"
    allowfullscreen="true" mozallowfullscreen="true" webkitallowfullscreen="true"
    style="position: absolute; top: 0; left: 0; width: 100%; height: 100%"></iframe>
</div>

## Stay informed

You can configure [Gerrit Notifications](https://fuchsia-review.googlesource.com/settings/#Notification)
to email you when new RFCs are uploaded.

Include the `docs/contribute/governance/rfcs` search expression
and select **Changes** to receive email notifications for
each new RFC proposal.

![Gerrit settings screenshot demonstrating
the above](resources/gerrit_notifications.png)

## Proposals

### Active RFCs

[Gerrit link](https://fuchsia-review.googlesource.com/q/dir:docs/contribute/governance/rfcs+is:open)

### Finalized RFCs

<div class="form-checkbox">
<devsite-expandable id="rfc-area">
  <h4 class="showalways">RFC area</h4>
<form id="filter-checkboxes-reset">
  {%- for area in areas %}
    {%- set found=false %}
    {%- for rfc in rfcs %}
        {%- for rfca in rfc.area %}
          {%- if rfca == area %}
            {%- set found=true %}
          {%- endif %}
        {%- endfor %}
    {%- endfor %}
    {%- if found %}
      <div class="checkbox-div">
        <input type="checkbox" id="checkbox-reset-{{ area|lower|replace(' ','-')|replace('.','-')  }}" checked>
        <label for="checkbox-reset-{{ area|lower|replace(' ','-')|replace('.','-') }}">{{ area }}</label>
      </div>
    {%- endif %}
  {%- endfor %}
  <br>
  <br>
  <button class="select-all">Select all</button>
  <button class="clear-all">Clear all</button>
  <hr>
  <div class="see-rfcs">
    <div class="rfc-left">
      <p><a href="#accepted-rfc">Accepted RFCs</a></p>
    </div>
    <div class="rfc-right">
      <p><a href="#rejected-rfc">Rejected RFCs</a></p>
    </div>
  </div>
</form>
</devsite-expandable>

<a name="accepted-rfc"><h3 class="hide-from-toc">Accepted</h3></a>
{% include "docs/contribute/governance/rfcs/_common/_index_table_header.md" %}
{%- for rfc in rfcs | sort(attribute='name') %}
    {%- if rfc.status == "Accepted" %}
        {% include "docs/contribute/governance/rfcs/_common/_index_table_body.md" %}
    {%- endif %}
{%- endfor %}
{% include "docs/contribute/governance/rfcs/_common/_index_table_footer.md" %}

<a name="rejected-rfc"><h3 class="hide-from-toc">Rejected</h3></a>
{% include "docs/contribute/governance/rfcs/_common/_index_table_header.md" %}
{%- for rfc in rfcs | sort(attribute='name') %}
    {%- if rfc.status == "Rejected" %}
        {% include "docs/contribute/governance/rfcs/_common/_index_table_body.md" %}
    {%- endif %}
{%- endfor %}
{% include "docs/contribute/governance/rfcs/_common/_index_table_footer.md" %}

{# This div is used to close the filter that is initialized above #}
</div>
